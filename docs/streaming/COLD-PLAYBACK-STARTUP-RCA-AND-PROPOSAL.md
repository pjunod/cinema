# Cold playback startup — why The Craft timed out and how to reach a few seconds

**Status:** revision 2; re-reviewed, findings addressed; the #1366 and #1376 repairs were deployed 2026-11-07 (media1/lab6 build `4ddbf1229`, lab4 `edf6d08fa`), remaining runtime work proposed ·
**Written:** 2026-11-02 EST · **Runtime investigated:** `ed708eda6b1df140626e9851bed6afbf06391f3f` ·
**Document base:** `c0f0051a1896245da5b12757a557fd3700147e5d` ·
**Owner:** Codex, branch `codex/playback-startup-rca`.

The user wants playback within a few seconds, preferably immediately. The
November 2 incident begins with unavailable exact preparation: a whole-file
source attestation resumed 0.66 s before the rolling producer installed.
That concurrent hash is the source reader recorded during the failed attempt.
Cold source reads and the 48-second media publication runway explain a long
start; a later controlled scanner pause/resume establishes a second source
contention mechanism. The scanner's presence during the original attempt is
unknown. Foreground copy is missing from the existing replicated source-I/O
ledger, although the CPU/GPU-only probe predicate was initially mistaken for
the place to add it. A prepared 720p trial presented in **3.855 seconds**;
a separate selected-audio identity defect blocked another retry.

Companion to [PLAYBACK](../PLAYBACK.md),
[the existing startup/P25 work](STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md) and
[the previous conservative disposition](PLAYBACK-STARTUP-LATENCY-BUILD.md#final-admission-disposition).
This is an RCA and proposed repair. The
[Opus/Fable dispositions](COLD-PLAYBACK-STARTUP-FABLE-REVIEW.md) supersede
revision 1's in-process arbiter and refusal paths. Retained observations and
limits are in [the incident packet](COLD-PLAYBACK-STARTUP-INCIDENT-20261102.json).

**Post-deployment retest:** [English AC-3 retest RCA](COLD-PLAYBACK-AC3-RETEST-RCA.md)
records deployed `8a212be7021f`, measured copy input starvation, continuous-
family pending/deletion, sparse-subtitle cancellation and cropped-UHD decode
policy defects, plus unsafe Resume handoff, silent presentation observation
and native replacement timeline ownership. It extends the proposed repair;
initial 720p continuity and
the historical 0.644x allocation remain incomplete. Astra reviewed it and
its findings are addressed. M2 is deployed; startup repairs remain proposed.

The [native callback RCA](NATIVE-CALLBACK-DELIVERY-RCA.md) establishes the
stranded subscription and measured renewal, with the reviewed lifecycle fix. The
[implementation draft](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md) now separates a
bounded native `observation_unavailable` notice from playback failure, without
pause or fabricated presentation/seek proof; review and validation are pending.

## 1. The failed path waits for work the prepared path avoids

The Craft, file 6964, is a 68,343,839,444-byte MKV containing 2160p HEVC,
about 90 Mb/s video, default English DTS audio, and an alternate English
AC-3 track. Safari resumed at film time 400.344 seconds on `media1`.
Hostnames and source paths in this document are sanitized for the mirror.

```text
Play original quality
  -> exact VOD/index unavailable: vod_index_pending / foreground_preempted
  -> rolling HEVC copy + DTS-to-E-AC-3 conversion
  -> writer accumulates its 12-second media gate
  -> publisher accumulates its separate 48-second media runway
  -> Safari's 40-second startup deadline expires before first playlist
  -> publisher later refuses the constrained pipeline's measured 0.818x rate

Play already prepared 720p
  -> immutable VOD attached in 673 ms
  -> first frame at 3,855 ms from the client's retry origin
```

**How to read this:** 48 seconds is a quantity of media, not a mandatory
48-second wall-clock sleep. A producer operating at 16x could accumulate it
in about three seconds, before other costs. A near-real-time producer cannot.
The recent rejection rate cannot reconstruct every preceding second.

### 1.1 Server and client observations identify the failure boundary

All timestamps below are UTC on November 2, 2026, equivalent to 09:06–09:07
EST for the original attempt. Server wall-clock events order the attempt;
the client TTFF below is a separate monotonic measurement. They are not
subtracted from each other.

| Observed event | UTC | Meaning |
|---|---|---|
| Decision: remux, unsupported DTS audio | 14:06:43.557 | Copy video; convert selected audio |
| Exact VOD refused | 14:06:43.875 | `vod_index_pending`, preparation queued with `foreground_preempted` |
| Full-source attestation resumes | 14:06:44.210 | Resumes at byte 7,918,845,952 of 68,343,839,444 |
| Copy producer installed | 14:06:44.874 | Actor-owned prepublication producer |
| Create request finished | 14:06:44.925 | 1,221 ms request budget accounting; `create` phase is a different origin |
| First complete segment | 14:06:48.233 | Writer elapsed 3,358 ms, segment duration 2.002 s |
| Writer gate opens | 14:07:00.734 | Writer elapsed 15,859 ms, accumulated 14.014 s of media |
| Safari reports startup exhaustion | 14:07:24.957 | No master playlist accepted before its 40 s attachment deadline |
| Publication clock retires stream | 14:07:39.005 | `rolling_insufficient_capacity`, measured 0.818x against 1.00x playback |

**Illustrative supply arithmetic:** 90 Mb/s is approximately 10.73 MiB/s.
The later 12.64 MiB/s cold-read control supplies about 1.18 times that rate;
48 seconds of media would take about 40.7 seconds to accumulate before
open/seek, rounding and presentation costs. This is a model using approximate
file demand and a later control, not a reconstruction of the original
variable-bitrate stream. The policy threshold divided by 54.13 seconds is
about 0.887; the exact staged inventory was not logged, so that quotient is
not an observed average rate. The actual logged 0.818x is a recent EWMA.

The retained events show approximately 40 seconds after attachment.
No successful original-quality presentation was observed.

The user later reported starting large movie transfers to the NAS external
disks, then clarified that they began only about 30 seconds before that
message, after the read benchmarks. Those transfers did not explain the
original failure or the retained experiments. No additional throughput
measurements were taken after that notice. Implementation qualification
must still record and control concurrent operator transfers.

### 1.2 Follow-up trials isolate route identity and source caching

English AC-3 track 4 then Resume failed immediately at 14:13:51.716 UTC with
409 `candidate_identity_absent`. Both catalogs reported 32 complete candidates;
decision logged audio 4, but request/response IDs were not captured.

The authorized prepared-720p retry attached VOD in 673 ms, presented 1,741 ms
after attachment and reported 3,855 ms from retry to first frame. This new
attempt does not erase the original wait or qualify continuity/original quality.

Production FFmpeg 8.1.3 processed the warm discarded-output control at 90.3x
with the same DTS conversion/pacing. That bypasses the daemon and benefits
from caching; the cold control below is decisive. Exact flags, positions,
byte counts and all measurement limits are retained in the JSON.

## 2. Confirmed causes — cold source I/O, admission and publication interact

### 2.1 The fast original-quality route was unavailable

[`attest_copy_source`](../../crates/plurxd/src/fragment_index_cluster.rs)
uses a whole-file digest for HEVC copy preparation. On the inspected node,
there was no attested source row for file 6964.
[`try_cluster_fragment_index`](../../crates/plurxd/src/vod/serve/construct.rs)
therefore could not reuse an exact cluster index, and
[`prepare_recipe`](../../crates/plurxd/src/vod/serve/create.rs) queued
preparation and returned `vod_index_pending` to select live recovery.

The source hash supports in-memory checkpoints. Restart loses partial hash
progress. Completing and preserving exact preparation before Play avoids
this work, but it cannot be presumed for a newly added title or cold node.
Changing full-source integrity into a partial hash would invalidate the
immutable-output proof and is not proposed.

A later read-only query found its fragment-index analysis request queued,
with `last_error_code=foreground_preempted`. The row's `priority=normal`
and `trigger=background` do not prove starvation: playback interest is a
separate expiring waiter, and source-worker admission considers live viewer
interest. Inspected waiters were cancelled after the attempts. Their state throughout the original attempt is unknown.

### 2.2 Two publication gates make a slow producer a slow startup

[`SessionDir::write_segment`](../../crates/plurxd/src/copyseg.rs) opens the
writer playlist after its 12-second media gate. That is not permission for
HTTP publication.
[`RollingStartupPolicy::Conservative`](../../crates/plurxd/src/transcode/rolling/publication.rs)
uses three times the fixed 16-second presentation target: 48 seconds at 1x,
relative to the accepted demand position. The publication worker in
[`publication_cycle_at`](../../crates/plurxd/src/transcode/rolling/session.rs)
withholds its first served snapshot until that runway is ready. HEVC master
construction also waits for an admitted init object through
[`segment_for_publication_before`](../../crates/plurxd/src/transcode/manager/publication.rs).
Opening the writer gate at 14 seconds of media consequently did not give
Safari a usable master playlist.

[`HLS_STARTUP.cold_deadline_ms`](../../crates/plurxd/src/web/playback-policy.js)
is 40,000 ms. Native readiness in
[`player.js`](../../crates/plurxd/src/web/player/player.js) bounds individual
fetches at 12 seconds within that fixed episode. The browser expired before
the server's capacity verdict. That is the confirmed cause of this observed
startup-timeout message. A longer browser timeout would expose a later
failure; it would not satisfy the few-second requirement.

The 48-second policy is deliberate, not an accidental leftover constant.
The [October disposition](PLAYBACK-STARTUP-LATENCY-BUILD.md#final-admission-disposition)
kept the shorter candidate test-only because transport identity did not
qualify source cadence, decoder behavior or continuity. It explicitly says
the couple-second target remains unfinished packaging/source work. This
incident reopens that missing product outcome without claiming the old
candidate has become qualified.

### 2.3 Cold source I/O reproduces the slowdown without the daemon

The original warm control was insufficient. The same production FFmpeg and
flags at a previously unused film position, 2400.344 s, produced its bounded
20-second sample at **0.987x**, writing to `/dev/null`. A raw read from a
fresh 256 MiB source window took 19.966 s: **12.82 MiB/s**. Rust parsing,
cache reservation and output writes were absent from both experiments.
They are therefore not necessary to reproduce the additional slowdown.

The NAS export is one external WD 20 TB ext4 disk, 98% occupied, USB 5 Gb/s,
`usb-storage`, queue depth 1, separate from the internal RAID pool. NAS-local
cold controls reached 28.58/29.48 MiB/s while other reads continued; the disk
was 99–100% busy at roughly 58–73 MiB/s total. Fullness/transport do not prove
fragmentation, drive failure or a defective cable.

A cross-client experiment distinguishes client caching from NAS-side work:

| Source region | First client, previously unused client window | Second client, same bytes and a cold client cache |
|---|---|---|
| 52 GiB offset, 64 MiB | `lab4`: 12.68 MiB/s | `media1`: 124.02 MiB/s |
| 54 GiB offset, 64 MiB | `media1`: 11.50 MiB/s | `lab4`: 272.16 MiB/s |

The order was reversed on the second region. These results establish a
NAS-side cache benefit across independent clients, rather than a daemon
conversion limit or a problem unique to the first client. The experiments
used fresh regions; neither NAS nor client caches were globally flushed.
An unobserved prior read cannot be categorically excluded, but the slow
first/fast second pattern repeated in both directions.

Secondary controls are retained in the JSON. Old NFS connections have an
asymmetric storage-NIC/LAN return path, but a fresh storage-source mount still
read only 7.92 MiB/s; it was removed afterward. Low ordinary/jumbo ping latency
does not explain cold disk service. Read-ahead 128 KiB / 8 MiB / 128 KiB gave
10.67 / 20.41 / 12.95 MiB/s, then the original 128 KiB was verified restored.
No permanent host tuning is proposed. Cache eviction pressure is unproven;
the later cache snapshot was about 19.68 GB against a 50 GiB ceiling.

### 2.4 A background scanner causes additional contention and cannot see copy playback

An all-process observation found a `scan_title` child of `plurxd`, nice 15,
reading a different title from the same external disk. Its active descriptor
advanced 113,348,217 bytes in about 3.13 seconds, approximately 34.5 MiB/s.
A stationary capability descriptor initially hid the active child reader.

A controlled A/B/A test used three fresh adjacent 64 MiB windows of The Craft:

| Scanner state | Cold source throughput | Wall time |
|---|---:|---:|
| Running | 12.64 MiB/s | 5.063 s |
| Stopped, same process identity | 24.33 MiB/s | 2.630 s |
| Resumed | 13.17 MiB/s | 4.858 s |

Only that background scanner was stopped. A pidfd bound the signals to the
observed process, a 15-second resume guard was armed, and `finally` resumed
it after the 2.630-second read. Its running state was verified afterward.
The playback service and production mount remained running. This is causal
evidence for scanner contention; it is not a claim that all other disk
activity was eliminated or that this scanner ran throughout the original
14:06 attempt. No per-process disk trace from that original interval exists.

The local probe monitor cannot see copy playback:
[`start_copy_with_audio_offset`](../../crates/plurxd/src/transcode/manager/start.rs)
takes no live CPU/GPU permit, even with audio conversion; `run_admitted_probe`
polls the CPU/GPU-only `fragment_worker_idle`. That predicate is not the
source arbiter and should not acquire source-demand semantics. Title
cancellation currently kills/reaps and yields without a checkpoint.

A second source-read conflict is deliberately allowed: failed VOD preparation
joins an expiring playback-analysis waiter, and the same playback ID then
starts rolling fallback without retiring that interest. While the media
session stays active, the waiter is refreshed; `analysis_source_may_continue`
in [state.rs](../../crates/plurxd/src/state.rs) permits full-source hashing
on an otherwise busy node when that viewer interest exists. The initial
incident log explicitly records the full-source attestation resuming.
The controlled scanner experiment does not quantify the hash's original
contribution, but the scheduling overlap is implemented and source-backed.

The shared arbiter already exists in
[`background_jobs_resources.sql`](../../crates/plurx-core/src/store/background_jobs_resources.sql)
and the corresponding analysis resource views. Background jobs, including
`DvTitleScan`, and source attestation reserve the same `source_io:<domain_id>`
keys, or global `source_io` when no mapping exists. Claim admission counts
two readers total and reserves the second for recognized durable live
consumers. Copy is an unregistered additional foreground reader. It is not
correct to describe the system as lacking a storage-domain model, or to
infer that scanner and hash occupied both slots during the original attempt.

A read-only production query at Unix second 1793634557 found **zero rows** in
`background_storage_domains`. Production therefore used the conservative
global fallback at that observation. Historical slot occupancy was not
captured. The existing GET/PUT `/api/v1/cluster/work/storage-domains` API owns
configuration; no mapping was changed during this investigation.

**Root-cause boundary:** cold backing-store reads and competing source work
are measured causes of low effective supply. The scanner's copy-admission
blind spot and the fallback's retained hash interest permit that competition.
The 48-second publication runway amplifies near-real-time supply into a long
startup, exceeding the browser deadline. Exact percentages for each cause
in the original attempt are unavailable; there is no evidence that merely
fixing cache inventory or buying an encoder would solve this incident.

### 2.5 Alternate audio produces inconsistent candidate identities

Decision discovery in
[`quality_catalog_from_snapshot_progress`](../../crates/plurxd/src/transcode/manager/candidates.rs)
normalizes the selected index but derives its default Copy tuple from the
file's default DTS track. Create discovery resolves selected AC-3 through
`AudioClaim`/`AudioRoute::Progressive`, including sink and offset restrictions.
The transcode bit participates in the recipe digest, so those routes get
different IDs. This source-confirmed parity defect is consistent with the
observed 409. The actual requested wire ID was not captured. M2 tests real
emitted IDs and canonical execution; it preserves stale-ID refusal.

## 3. Product contract — start playback; arbitrate preparation behind it

The proposed targets are **2 s for ready output** and **5 s for supported
cold starts**, measured from the original Play/Resume gesture to first
presented media, including decision, registration, preparation and attach.
They remain unmet acceptance objectives, not achieved fleet guarantees.
Report counts, distributions, maximums and every failed attempt by source
class, supply envelope and transport. Successor clocks never erase an
initial wait. A fast refusal never counts as a fast start.

A missing index selects rolling recovery. Source arbitration must not invent
a `startup_source_busy` response, a quiescence-timeout start refusal, or a
rule that another viewer's preparation can prevent Play. Ordinary auth,
source-change, authority and actual I/O failures retain their existing
contracts. Registration cannot bypass the serving/owner fence during quorum
loss. Those failures are not reclassified as successful startup.

Cold throughput below the source's sustained consumption rate cannot support
uninterrupted original playback indefinitely from a small bootstrap. Encoding
that same cold input still reads its source bytes. A compatible prepared
rendition can avoid those reads; silently replacing explicit Original is
not proposed. The current `rolling_insufficient_capacity` policy remains
unchanged until the user decides whether insufficient supply should retire
or continue with visibly attributed rebuffering. That is a product decision,
not a hidden scheduler default.

The fix has three coupled lanes: source arbitration/fair progress, mandatory
first-window packaging, and ready-path latency. Selected-audio recipe parity
is an independent correctness fix and can ship separately. No Developer
switch is needed for these repairs.

## 4. M0 — own both cold first-packet delay and the ready-path budget

Retain monotonic boundaries for gesture, decision, source-demand registration,
producer spawn, source open, demux/probe reads, seek, first packet, first
complete segment, writer publication, served snapshot, HTTP admission,
attach/append/decode and first presented media. Attribute source RPC offsets,
bytes and waits separately from sequential supply. Join client/server phases
by request/attempt identity without subtracting unrelated clocks. Existing
source/cancellation/authority fences still govern the trace.

The observed decision-to-create interval is about 1.37 s and producer-install
to first complete 2.002 s segment is 3.358 s. These are different origins;
do not add the 1.741 s ready-trial attach phase as if it were the cold trial's
measured decode time. They nevertheless show that changing a gate alone
cannot be assumed to satisfy 5 s. Separate open/seek/probe from productive
media supply before choosing the repair.

**Cold-seek lever:** inspect actual MKV header/SeekHead/Tracks/Cues or MP4
metadata reads. Existing output-fragment indexes do not store original
container byte offsets, and the sparse subtitle cue reader is not a persisted
video seek map. If those reads dominate, implement a source-version-bound
header/cue/keyframe map with an actual demux consumer: validated metadata
prefetch, custom I/O or another explicitly reviewed integration. Storing
unused offsets is not a fix. Revalidate object identity and parser/pipeline
version before reuse; account for both cold misses and warm hits. Do not
fabricate an exact immutable index from this startup metadata.

**Ready-path owner:** the 3.855 s ready trial misses 2 s. Capture and remove
its actual serial work: 673 ms VOD creation plus 1,741 ms attach/presentation
already exceeds the goal, with additional gesture-to-attach work unaccounted
for. M0 includes the resulting client/server corrections and matched
before/after measurements. M1 or M3 cannot automatically claim that outcome.

**Acceptance:** source-exact cold and ready traces account for their critical
paths and name each removed wait. Include scanner/hash phases and operator
workload. No new throughput experiments run while the user's movie transfers
are active. The retained pre-transfer experiments remain valid with their
stated background-work limits.

## 5. M1 — extend the existing replicated source ledger

### 5.1 One resource authority, including foreground readers

Reuse these current contracts; re-verify them at the implementation base:

| Existing interface | Responsibility |
|---|---|
| `background_storage_domains` and `background_job_required_resources` in [resources SQL](../../crates/plurx-core/src/store/background_jobs_resources.sql) | Root aliases map to physical source domains; unmapped roots use global `source_io` |
| Analysis resource views in [viewer-analysis SQL](../../crates/plurx-core/src/store/background_jobs_viewer_analysis.sql) | Attestation and background work share domain keys |
| `CLAIM_SQL` in [background jobs](../../crates/plurx-core/src/store/background_jobs.rs) | Shared reservation counts and typed durable live-consumer classification |
| [Storage-domain API](../../crates/plurxd/src/http/background_jobs.rs) | Atomic mapping replacement; 409 while affected live ownership prevents reinterpretation |
| [Job cleanup owner](../../crates/plurxd/src/jobs.rs) and [bounded child owner](../../crates/plurx-core/src/process/bounded.rs) | Physical child settlement precedes returned capacity |

Add a typed foreground original-source owner/projection to this resources
module, using the same domain resolution, claim predicates and holder reports.
The existing background reservation table references background jobs and has
unique slots: a copy reader must not be a fake job or mislabeled
`playback_fragment`. A proposed foreground owner binds playback/session
incarnation, producer attempt, node/boot, source generation, resource keys,
phase and lease fence. This is an extension of the same arbiter, not a
separate in-process lease or counter authority.

Register foreground demand before original-source input access. One atomic
Raft transaction establishes its exact owner and closes new maintenance
admission on those keys; publish a wakeup to existing readers, with polling
as recovery rather than correctness authority. Measure and budget this write
in M0, with a proposed <=100 ms healthy-cluster registration objective. Do
not add replicated writes per packet, segment or HTTP chunk. Existing
writer/apply latency and authority-loss handling remain visible in TTFF.

Production currently has no domain mappings. Inventory real roots and
backing volumes and use the existing API to configure aliases safely when
its live-reservation guard allows; do not infer a physical disk from a
client-local device number or equate every NAS export. Extend replacement
protection to foreground and suspended ownership. The global fallback remains
safe but conservative until configuration is qualified. There is no deferred
need to invent another topology model.

### 5.2 Start immediately after registration; retain the old cleanup charge

The producer starts after its registered demand without waiting for a
background reap acknowledgement. Signal source readers to yield; keep their
claims charged until their owned pause/cleanup transition actually settles.
The first proposal's 1 s quiescence wait and both viewer-facing source-busy
refusals are removed.

This explicitly changes admission accounting during preemption: the two-reader
limit continues to constrain maintenance admission, while a registered
foreground owner may temporarily overlap already charged readers. Report
that overlap in the same holder ledger; never steal their slots or pretend
cleanup completed. Scope an overlap to the exact live foreground generation,
expire abandoned demand, and prohibit new maintenance while it exists.
A stale acknowledgement cannot start a successor or release another owner's
claim. An unreapable reader remains visible as `cleanup_pending` with an
existing authorized Activity stop/cancel action; stop never fabricates reap
or free credit. QoS degradation remains observable instead of becoming a new
Play refusal. Registration/authority failures use existing recovery rather
than an unowned read.

Background cancellation currently retains reservations in `cancelling`;
ordinary settlement deletes them, so settle only after physical join.
Analysis cancellation/retry retains its source reservation until expiry;
successful publication releases it. Preserve those distinct contracts and
bind all releases/renewals to their exact generation. Foreground ownership
survives retries and failed reap; returned HTTP responses, registry removal
and cancelled awaiters are not cleanup proof.

**First isolated correction:** make the VOD-to-rolling transition update the
exact playback-analysis interest and consult this shared arbitration in
`analysis_source_may_continue`. The original attempt actually logged that
hash resuming. Keep preparation durable and restore correct interest if the
fallback retires. Another live preparation waiter is not permission to bypass
foreground demand. A viewer still needing exact preparation uses rolling
recovery when possible; source scheduling alone cannot add a refusal.

### 5.3 Startup priority ends; sharing is path-specific and domain-wide

[The source-sharing contract](COLD-PLAYBACK-SOURCE-SHARING.md) owns the
rolling high/low state, finite byte-service policy for progressive/direct
bodies, slow/idle consumers and range/seek transitions. No film-lead signal
is invented for a path that lacks it; legacy/non-telemetry delivery uses
an explicit transfer service class. Missing rolling observations cannot
become permission to compete.

A domain decision considers every current foreground generation: any
startup/refill owner blocks background resume; every rolling owner must
permit sharing with fresh observations, and every transfer owner must have
a valid protective service grant. Atomic resume revalidates mixed owners,
remote generations, all domain keys and available occupancy. One high owner
cannot override another low or starting owner. M4 must prove preparation
advances during sufficiently provisioned long direct/progressive transfers,
not just rolling sessions. Neither a lifetime copy lock nor a hidden source
refusal satisfies that requirement. Source arbitration stays outside the
CPU/GPU-only `fragment_worker_idle` predicate.

### 5.4 Suspend checkpointless scans with owned process-group state

For source-pressure yield, add pause/resume to the existing owned child-group
lifecycle rather than treating SIGSTOP as job settlement. Pause the group,
not just its leader; retain the collector, descriptors, job claim and cleanup
owner. Verify the stopped acknowledgement. A thread stuck in uninterruptible
I/O is still active and charged until proven stopped or reaped; foreground
start still uses the explicit overlap rule.

Add an atomic paused-source transition that can return the active source-I/O
slot while retaining the fenced running job and its durable paused owner.
Its resource keys remain fixed, mapping replacement remains protected, and
holder reports show both paused ownership and active occupancy. Resume must
reacquire a source slot under the same current claim before SIGCONT and
revalidate source/tool/claim identity. It is invalid to `Yield`, release the
job fence and later resume that old process. Heartbeats continue while paused;
lease loss, cancellation, source/tool change or shutdown kills/reaps the
owned group before releasing remaining ownership.

Separate active execution timeout from paused time, while retaining a bounded
wall-age/abandonment policy and visible pause age; the existing title scan's
six-hour wall timeout cannot silently fire just because it yielded. CPU/GPU
preemption remains its own rule. Regression coverage must exercise group
children, stalled stop acknowledgement, cancellation while stopped, failed
reap, stale resume, expiry and restart recovery. The diagnostic A/B/A proved
I/O relief, not this new lifecycle or a completed implementation.

### 5.5 Persist whole-file hash continuation and bound source prefetch

Make resumable hash persistence part of M1. Existing memory checkpoints store
`FullDigestKey`, byte offset and cloned `Sha256`, at 64 MiB boundaries with
256 KiB reads, bounded to 16 entries. Deploy/restart loses that work. Preserve
the full whole-file SHA-256 identity: chunk digests cannot be combined into
the same digest. `sha2 0.10` has no stable automatic state serialization.

Use a versioned, explicitly defined continuation format: algorithm/regime,
chaining state, total byte count, partial block, offset and full source key
(node, file, object version including size/inode/device/mtime/ctime precision).
Persist an owner-only atomic artifact on durable node storage, with fsync and
a durable job checkpoint reference after the rename. Bound retention, clean
only unowned orphans, and validate corruption, version and source identity
before resume. A checkpoint is only resume input; full EOF and unchanged
source checks are still required before publishing an attestation. Golden
continuation vectors must match the existing uninterrupted implementation
across chunk boundaries and restart; corrupt/stale states rebuild safely.

Measure a daemon-owned bounded input-prefetch experiment using the held source
capability and sequential advice/read windows. It belongs to the same source
owner and pacing budget; it cannot become an extra uncounted reader, read the
whole title on Play or hide prefetch time before the gesture origin. Compare
cold matched windows, cancellation, memory/byte bounds and source replacement.
The 128 KiB/8 MiB host read-ahead experiment is motivation, not a default to
ship or permission to change permanent host settings.

### 5.6 Register real foreground reads, not cached HTTP traffic

| Foreground path | Registration owner |
|---|---|
| Rolling copy and encoded producers | Original-source producer attempt, including initial and replacement spawn |
| Ordinary progressive `/stream.mp4` | Held original-source/remux body producer until child/source closure |
| Exact-copy or encoded VOD generation | Materializer's original-source descriptor/FFmpeg generation, not every cached segment GET |
| Shared direct original-file ranges | `source_file_body` read custody, including outstanding blocking reads at cancellation |
| Shared/cached HLS and retained VOD delivery | No new original-source demand when serving materialized bytes; preserve existing cache/read guards |

`SourceReadJobGate` in the supplied review is a test-only synchronization
seam. Production uses its response guard and local read-job semaphore; those
do not replace the replicated ledger. Inventory all callers at build time,
including original direct-range serving. Exclude a path only with a concrete
proof that it reads prepared output instead of the original source.

**M1 acceptance:** SQLite and feature-qualified replicated Store tests prove
atomic shared-domain admission, priority overlap accounting, paused-slot
release/reacquisition, distinct waiter preservation and safe exact-generation
cleanup. Child tests prove pause/resume and reap ownership; restart tests
prove hash continuation and invalidation. Runtime traces show the first
producer starts without a background-reap wait, source contention recedes,
and preparation advances during qualified steady playback, including another
node's reader. Keep storage ownership, provider limits and ordinary background
progress intact. No runtime changes in this proposal are already qualified.

## 6. M2 — ship selected-audio identity parity independently

Use the canonical selected-track `AudioClaim`/`resolve_audio` result with
`AudioRoute::Progressive` and the actual offset in decision and create
catalogs. Preserve explicit retained recipe and reviewed video conversion
semantics; no looser matching of a stale candidate ID.

The existing regression is in
[HTTP HLS chunk 06](../../crates/plurxd/src/http/hls/tests/chunk_06.rs), not
the similarly named transcode test file. Its create-to-execution test misses
decision discovery. Add a regression comparing actual emitted decision and
create IDs/digests for alternate compatible AC-3 over default DTS, reversed
compatibility, offset, sink-channel/sample-rate limits, defaults and another
selection's stale ID. Preserve no-claim legacy behavior. Compile and run it
against old/new code so failure is at the identity assertion, not helper
absence. This lane has user authorization for implementation in the separate
`codex/audio-candidate-parity` worktree; its actual result belongs in §9.

## 7. M3 — mandatory short-window publication alongside M1

M1 alone cannot meet 5 s: even at approximately 2.3x the 48 s runway costs
about 21 s. Run M3 alongside source arbitration. Vary both the writer's
12-second media gate and the publisher's 48-second first runway, as well as
advertised target and subsequent publication cadence. Keep both old gates
as controls; changing only the latter still leaves the writer floor.

Include a **one complete independently decodable segment** bootstrap arm
with validated init and complete fragment, alongside 4/4, 4/8 and existing
lab controls. The observed segment was 2.002 s, not exactly 2 s for every
source. The current lab requires bootstrap at least two targets; extend that
experiment rule explicitly for this arm and retain the existing rule for
old arms. A lab admission is not production qualification. Copied long/open
GOPs cannot be made independently decodable by changing a constant.

Define each arm's source class, supply ratio, seek/metadata strategy, native
holdback, bootstrap, cadence, retention and unchanged steady reserve. Run
M0's cold seek/prefetch lever where needed. Qualify init, first frame and
subsequent continuity together. Reject invalid target durations, missing
random access or underflow rather than weakening an invariant. Generated
fixtures alone cannot admit a real daemon/source/native-player class.

**Acceptance:** measured cold starts reach the proposed <=5 s target under
the declared supply/source/transport envelope while keeping playback stable.
If a class cannot meet it, retain its actual unmet result and identify the
next concrete lever. Do not label it accepted, silently exempt it, or claim
that a 4/4 arm estimate is an observed 5 s result. No change to steady
capacity retirement/rebuffer policy is implied.

## 8. M4 — qualification includes latency, continued playback and preparation

| Dimension | Required cases |
|---|---|
| Availability | Ready exact copy/encode; cold copy/encode; missing index; preempted/restarted preparation; retained cache |
| Clients | Native Safari and hls.js, then affected Apple/Android surfaces before all-client claims |
| Source | This high-bitrate HEVC MKV; H.264; affected HDR/DV classes; short/open/long GOP; near EOF |
| Intent | Play, Resume, seek, quality/audio change, pause; explicit Original distinct from Auto; shared original ranges |
| Storage pressure | Hash plus scanner; another node; mapped aliases/global fallback; operator transfers measured separately; near-ceiling cache |
| Lifecycle | Immediate start with old cleanup pending; failed reap; stopped/expired owner; two waiters; overlapping copies; source replacement; restart |
| Continuity/fairness | Thirty minutes or EOF; sufficient 1x/2x supply; pause/refill; steady hash/scanner progress; separate insufficient-supply recovery arms |

At least ten cold and ten ready starts per admitted source/transport class,
with no discarded attempts. Retain distribution/maximum and every failure.
A successful 2x playback cell requires measured supply sufficient for 2x plus
its qualified scheduling/append allowance; 1.05x supply at 2x is a refusal
or recovery test, not a possible sustained-success requirement. Keep original
gesture clocks through route successors. Report actual served audio/range,
first frame, early stalls, source overcommit intervals and background progress.

M0's <=2 s ready result and M3's <=5 s cold result need their own receipts.
A quick error, warm-only result, missing cell or the single 720p trial is not
universal acceptance. Keep lab, physical-device and deployed fleet results
separate. Existing longer continuity requirements continue to apply.

## 9. Integration, review and execution ledger

The supplied Opus 5.5 review plus independent Fable 5.1 review is retained with
[dispositions](COLD-PLAYBACK-STARTUP-FABLE-REVIEW.md). It rejects the first
proposal's new in-process arbiter, refusal paths and optional M3. The prior
[Astra review](COLD-PLAYBACK-STARTUP-ASTRA-REVIEW.md) remains historical review
of revision 1; its conservative refusal resolution is superseded here.
Astra re-reviewed revision 2; its two sharing-policy findings are addressed
in the source-sharing contract and retained in the review/disposition record. No startup runtime change
is approved or qualified by these documents.

Use the existing source-I/O/resource and startup/P25 projects rather than
inventing competing authorities. Build the hash/fallback coordination first
as a focused change, but integrate M1 and mandatory M3 on one effort branch
with explicit ownership. The independent M2 correctness lane can target
main with its own regression and gate. Preserve current developer/feature
policy: no gratuitous toggle. Before build, re-verify source contracts and
run scoped knowledge context/obligations on the actual diff; empty pilot
coverage is not a waiver. Use the pinned compiler and normal tracked hooks.
Never run uncapped Rust builds on a Raft voter.

| Item | Actual state |
|---|---|
| Original attempt | Timeout before playlist; full-source hash resuming is logged; later 0.818x capacity refusal; no first frame |
| Scanner control | Later A/B/A proves contention; presence at original attempt not captured |
| Warm/cold controls | Retained in the JSON; timestamps not invented; user transfers began afterward |
| Prepared 720p | One first frame at 3.855 s; misses proposed ready target; continuity not qualified |
| Source model/config | Existing replicated ledger; zero production mappings at Unix second 1793634557, global fallback in use |
| M1/M3 | Reworked proposal; no runtime implementation or acceptance claimed |
| M2 | Merged via [PR #1356](http://forge.lan:3000/noirr/plurx/pulls/1356), landing `8a212be7021f` (audio commit `1277378dc`, same two Rust files as reviewed `860973662`); old/new regression controls, pinned check/Clippy and exact-head Main promotion gate passed; user-deployed to the fleet; `media1` image/build verified at `8a212be70`, AC-3 selected and accepted |
| Review | Opus/Fable R1 incorporated; Astra revision-2 re-review completed, two findings addressed |
