# Cold startup proposal — Opus/Fable review and revision 2 dispositions

**Status:** done · **Scope:** supplied review and proposal dispositions ·
**Received:** 2026-11-02 EST · **Reviewers reported by the user:** Claude
Opus 5.5 plus an independent Claude Fable 5.1 pass, merged ·
**Review basis:** documentation `ddbd6c2a3`, runtime `ed708eda6`.

Companion to [the revised RCA/proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md),
[the historical Astra review](COLD-PLAYBACK-STARTUP-ASTRA-REVIEW.md) and
[the incident packet](COLD-PLAYBACK-STARTUP-INCIDENT-20261102.json).
The review below is user-supplied reviewer output, not a review invoked or
independently authenticated by Codex. It reports read-only source checks and
no runtime tests/builds. Codex verified the material source contracts before
making these dispositions. Revision 2 requires fresh independent review;
this record does not approve startup runtime implementation.

## 1. F1 / P1 — reuse the existing source-domain ledger

**Accepted.** The first proposal missed the implemented shared resource
model and incorrectly proposed another local authority. M1 now extends the
same `source_io:<domain>`/global-fallback resolution, reservation observations
and claim predicates with typed foreground source owners. It explicitly
forbids fake background jobs or copying the `playback_fragment` label onto
unrelated live readers. The existing background reservation table has job
foreign keys and slot uniqueness; those constraints cannot simply accept a
media session ID.

A read-only production query at Unix second 1793634557 found no configured
domain mappings. The global fallback is in force at that observation. This
is a configuration fact, not proof of historical slot occupancy. Existing
GET/PUT storage-domain interfaces remain the configuration authority; live
foreground and suspended ownership must join their replacement guard.
Registration adds one measured/fenced Raft transaction, with no writes per
packet or segment. The proposed healthy-cluster budget is <=100 ms and is
an acceptance objective, not an observed result.

`fragment_worker_idle` remains CPU/GPU admission. Source-pressure decisions
move through resource-aware reader ownership and shared domain observations.
The original hash is the first focused correction because it is the reader
logged at the failed attempt. The scanner is a later causal reproduction,
not a retrospectively proven participant in that attempt.

## 2. F2 / P1 — remove both background-race start refusals

**Accepted, with explicit accounting.** Remove `startup_source_busy`, the
1-second background quiescence wait, and the priority table that could deny
a viewer because another viewer needs preparation. After registered source
demand, start rolling without waiting for background reap; missing exact
preparation already selects live recovery. Ordinary source/auth/authority
failures retain their own contracts.

The existing hard two-reader policy and an immediate third foreground read
cannot both remain unchanged while old work is still charged. Revision 2
therefore specifies **accounted foreground-preemption overlap**: maintenance
admission is blocked atomically, old owners stay charged, and the foreground
owner appears in the same resource holder view. It must not fabricate freed
slots or unconditionally release a stuck child's lease. Unreapable work is
visible with the authorized Activity stop/cancel control; the action does
not manufacture cleanup credit. Retrying/abandoned foreground generations
remain fenced.

## 3. F3 / P1 — make M3 mandatory and remove both startup gates

**Accepted.** M3 runs alongside M1 and includes the 12-second writer gate,
48-second first-publication runway, cadence, native holdback and steady
reserve. It includes a one-complete-segment arm; the observed segment is
2.002 seconds, not a universal exact two-second cut. The lab's existing
bootstrap >= two-targets rule needs an explicitly reviewed experimental
extension for this arm. That is not production qualification or permission
to weaken media validity.

M0 separately times open, seek, demux/probe reads and first packet. A
source-version-bound container header/cue/keyframe map is a named lever,
with actual demux/custom-I/O or metadata-prefetch integration required.
Existing output segment indexes and the subtitle cue reader are not that
persisted video map. The 2 s ready and 5 s cold outcomes stay per-class,
supply-conditioned acceptance objectives; unsupported estimates are not
claims that the target was achieved.

## 4. F4 / P2 — make steady preparation progress and restart continuation owned work

**Accepted, with lifecycle prerequisites.** Copy gets startup/refill priority,
then shares by validated producer lead and high/low hysteresis. M1 owns
background progress measurement and copy pacing once reserve is banked;
FFmpeg's fixed readrate flag is not assumed dynamically editable. Intentional
scheduler holds must not be mistaken for low unconstrained production speed.

For source-pressure yield, an owned process-group pause replaces destruction
of checkpointless scans where safe. Current cancellation still kills/reaps;
there is no existing production paused-resource state. The new transition
retains the fenced running job, collector, heartbeat and cleanup owner,
returns active source occupancy only after confirmed stop, and reacquires
that slot before validated SIGCONT. It cannot settle `Yield` and resume an
old released fence. Stalled stop uses the explicit overlap rule; authority
loss or invalid source/tool identity uses kill/reap.

Paused time needs separate execution/lifetime accounting and bounded orphan
cleanup. Source arbitration cannot use an unowned PID or leader-only signal.
The diagnostic pause control demonstrated performance relief, not this
shipping lifecycle. Tests must cover descendants and cancellation while
stopped, stale resume, expiry and failed reap.

Durable hash continuation is now mandatory M1 scope. Persist a versioned,
validated SHA-256 continuation and source key atomically on durable storage,
with a durable job reference. `sha2 0.10` has no stable automatic serialized
state. Separate chunk digests are not the existing whole-file digest.
Checkpoint data remains untrusted resume input until validation; full EOF
plus unchanged source still precedes attestation publication. Continuation
vectors, crash/restart and corruption/invalidation tests are required.

## 5. F5 / P2 — cover actual foreground source readers

**Accepted with one source correction.** The review's `SourceReadJobGate`
reference is a test-only seam. Production shared original-source bodies
use a response guard and local read semaphore, not that gate. Revision 2
inventories rolling/encoded producers, progressive remux, shared original
ranges and VOD materialization. Register at the real original-source owner,
including outstanding blocking reads during body cancellation. Cached or
retained HLS bytes do not become another original-source reader on every GET.

The inspected exact-copy VOD path materializes output from a held original
source via FFmpeg. The inspected HTTP path serves materialized/retained
artifacts; it is not raw source-fragment zero-copy serving. Other direct
range callers must be inventoried rather than assumed covered.

## 6. F6–F7 / P2 — weight the original hash and measure the cheap source lever

**Accepted.** The summary now leads with the full-source hash resumed 0.66 s
before producer installation. It distinguishes that original evidence from
the later scanner A/B/A. The fallback/analysis interest transition is the
first isolated correction within the shared arbiter.

Below the sustained input consumption rate, encoding the same original does
not create missing source bandwidth. Keep prepared-output alternatives and
the insufficient-supply product decision explicit. A bounded daemon-side
sequential advice/prefetch experiment is part of M1, charged to its input
owner, with cancellation, byte/memory bounds and matched cold controls. It
does not authorize host tuning or whole-file warmup outside the TTFF clock.

## 7. Arithmetic and evidence corrections

The 90 Mb/s demand and later 12.6 MiB/s control imply approximately 1.18x and
about 40.7 s for a 48 s runway. Revision 2 includes that as **illustration**,
not a measured replay: source/selected-output bitrate, seek/probe and segment
rounding differ. The supplied assertion that the pipeline *observably*
produced 48 s over 54.1 s is too strong. Forty-eight seconds is a policy
minimum at the prepublication capacity check; actual staged inventory and
its crossing time were not logged. The quotient is not an exact observed
average and must not replace the logged 0.818x recent EWMA.

The retained experiments preceded the user's later movie transfers. Do not
reinterpret that later workload as the cause of the original attempt.
Further performance qualification waits for controlled/recorded operator
workload; no new media benchmarks were run for this revision.

## 8. P3 follow-ups and re-review

- **Independent M2:** the user explicitly authorized implementation after
  this review was received. Its isolated branch is
  `codex/audio-candidate-parity`, audio commit `1277378dc`
  (same two Rust files as reviewed `860973662`),
  [PR #1356](http://forge.lan:3000/noirr/plurx/pulls/1356). The new regression
  failed at the actual ID assertion on old code and passed after the fix;
  existing parity controls, pinned all-target check/Clippy and normal hooks
  passed. Astra found no actionable defect. The exact-head Main promotion
  gate passed, and PR #1356 merged as `8a212be7021f`. This is separate from
  implementing M1/M3 and is not deployed.
- **Ready path:** M0 owns both the 673 ms create and 1,741 ms attach/presentation
  contributions plus the unaccounted gesture interval. They do not sum to a
  two-second result merely because the source is ready.
- **Storage hardware:** extent layout and whether this enclosure supports
  UAS remain read-only diagnostic follow-ups. The observed `usb-storage`
  transport and fullness do not establish fragmentation or defective hardware.
  No transport switch or performance test is done during the user's transfers.
- **Insufficient capacity:** the existing retirement is source-backed. Changing
  it to continuing rebuffering needs the user's product decision; revision 2
  does not change that behavior silently.

A fresh independent review must assess the shared-ledger extension,
foreground overlap, paused-claim accounting, durable continuation and mandatory
one-segment startup. The previous Astra record remains a historical review
of revision 1; its refusal-policy disposition is superseded.

## 9. Astra revision-2 re-review and dispositions

Astra performed a fresh read-only source/document review after the changes
above, with no live commands or test execution. It found no actionable defect
in the separate two-file audio patch and no additional blocking evidence issue.
It identified two remaining P2 proposal gaps:

**R2-A1 — nonrolling fairness:** progressive/direct owners lack rolling
presentation/film-lead observations, so missing telemetry must not become
lifetime startup priority. Addressed in the
[source-sharing contract](COLD-PLAYBACK-SOURCE-SHARING.md): explicit transfer
service after a bounded first read, finite byte-service epochs, known-floor
and unknown-floor treatment, slow/idle consumer custody, new ranges/seeks and
non-telemetry classification. Long progressive/direct transfers now have
owned background-progress acceptance cells. Proposed grant parameters remain
qualification hypotheses, not measured defaults.

**R2-A2 — aggregate foreground owners:** one high-lead viewer must not resume
background work while another is starting/refilling. Addressed by domain-wide
conjunction over all current generations, mixed path classes and resource
keys, with atomic observation/occupancy revalidation at resume. Stale high
observations cannot override fresh startup/refill demand. The contract names
local/remote, stale, mixed-type and overlapping-viewer regression/runtime cells.

Astra's disposition was to resolve these two points before the next review
handoff; it did not approve runtime implementation or independently execute
the audio regression. Codex made the dispositions above after that review.
No third full Astra review is claimed.

## 10. Supplied merged review, retained verbatim

The following text is reviewer input, not additional verified execution.

<details>
<summary>Opus 5.5 and independent Fable 5.1 review supplied by the user</summary>

# Cold playback startup — review of the RCA/proposal packet (2026-11-02)

**Reviewers:** Claude Opus 5.5 and an independent Claude Fable 5.1 pass, merged.
**Basis:** packet (docs commit ddbd6c2a, checksums verified). Source spot-checked read-only at runtime ed708eda6.
**Verdict:** Keep the RCA. Do not build the proposal as written. Rework M1, drop both refusal paths, make M3 mandatory, then re-review.

## RCA: confirmed
- These packet claims match the source:
  - `fragment_worker_idle` (transcode/manager/construct.rs:1488) checks CPU/GPU/offline only. It does not see copy readers.
  - `analysis_source_may_continue` (state.rs:10545) lets any live playback waiter keep hashing on a busy node.
  - The 48 s Conservative runway holds.
  - The capacity check retires the stream when `speed + 0.05 < rate` (rolling/flow.rs:279).
- The arithmetic supports the chain:
  - Demand is ~10.7 MiB/s (90 Mb/s).
  - Cold supply with the scanner running is 12.6 MiB/s, about 1.18x.
  - 48 s of runway at 1.18x takes 40.7 s, about Safari's 40 s deadline. Add this to §2.
  - The observed pipeline ran at 0.88x: 48 s of media over 54.1 s, from producer install to retirement.

## P1: fix before build

**1. M1 duplicates an arbiter that already exists.**
- The replicated Store already has `background_storage_domains`: "Roots on the same physical storage share a domain regardless of mount alias" (plurx-core/src/store/background_jobs_resources.sql:1-20).
- Background jobs and attestation already reserve `source_io:<domain_id>`.
- Claim admission caps each key at 2 readers. The second reader is reserved for live demand, meaning `playback_fragment` consumers (background_jobs.rs:591-614).
- The observed scanner is a `DvTitleScan`, claimed through `claim_with_resolution` (source_probe/title_work.rs:140). It already holds a slot.
- Likely incident state: scanner and hash on the two counted slots, plus rolling copy as an **uncounted third reader**.

Required:
- Register the copy/rolling session as a live-demand consumer in this ledger, bound to its playback/attempt generation.
- Drop the new in-process lease.
- Do not add disk demand to `fragment_worker_idle`. That gate is CPU/GPU, and many consumers that never touch the source disk poll it.
- State whether production domain mappings exist. They are set via `PUT` (http/mod.rs:1938).
- Budget the Raft write latency on the startup path.
- This also removes the deferred "cross-node topology design" item. The domain model and the replicated ledger already exist.

**2. Both refusal paths invert the product requirement.**
- Requirement: any title plays through rolling or VOD, and a missing index is not a reason to fail.
- `startup_source_busy` (503) for a required-preparation waiter, and "nonpreemptible work / failed cleanup → unavailable", both let background work or another viewer fail a Play.
- Supply exists: one copy needs ~10.7 MiB/s, and a lone reader gets 24–29 MiB/s.
- The 1 s quiescence wait spends up to half the ready budget to save a few hundred ms of shared bandwidth.

Required:
- The arbiter decides who yields. It never emits a viewer-facing start failure.
- Block new background admission synchronously, and signal existing readers.
- Start the producer immediately. Do not wait for reap.
- The cleanup owner keeps the fence.
- A viewer who loses the source race gets rolling fallback. `prepare_recipe` already demotes `vod_index_pending` to live recovery.
- Surface an unreapable background reader in the UI with a stop control.

**3. The 5 s cold target is unreachable on this title with the listed milestones.**
- Decision→create took 1.37 s. Spawn→first complete 2 s segment took 3.36 s. That is ~4.7 s before any publication policy, plus ~1.7 s attach→present.
- With the scanner stopped (2.3x), the 48 s runway alone is ~21 s.
- The proposed 4 s/4 s arm estimates at ~5.4 s.
- Cold NFS reads were ~100 ms/op at 128 KiB read-ahead, so seek/probe random reads likely dominate the first segment. This is inference.

Required:
- M3 is mandatory, not "only if still necessary". Run it alongside M1.
- Add a one-segment (2 s) bootstrap arm.
- Put the separate 12 s writer gate (§2.2) in M3 scope. It is a floor too.
- M0 times spawn/open/seek/probe-read/first-packet separately from supply.
- Name a lever for that interval, e.g. a persisted per-file container seek map, so cold seeks need no cold random reads.
- Otherwise, state targets per source class, conditioned on measured supply ratio.

## P2

**4. Lifetime copy protection starves the work that makes starts warm.**
- This title was new: source mtime is 2026-11-01 23:51 UTC, ~14 h before the incident.
- A whole-file hash needs ~37 min at NAS-local speed and ~85 min at cold NFS speed.
- Restart loses hash progress, scans have no checkpoint, and deploys are frequent.
- Under steady viewing, new titles never become ready.
- Kill/reap of a checkpointless scan throws away bytes already read, which then get read again.
- The copy's `-readrate 2.00` (~21 MiB/s) leaves ~nothing for the hash at ~24 MiB/s supply.

Required:
- Give copy priority during startup only.
- After startup, admit background readers by producer lead, with hysteresis: admit above a high watermark, preempt below a low one.
- Lower the copy read rate once the runway is banked.
- Use SIGSTOP/SIGCONT of the child group as the scan-yield primitive; the A/B/A showed it is instant and lossless.
- Make hash checkpoint persistence (chunked) part of M1, not a deferred integrity design.

**5. Demand coverage stops at `start_copy_with_audio_offset`.**
- Other foreground source-read paths exist: http/shared_source_playback.rs:1291 (`SourceReadJobGate`) and http/shared_source_direct.rs.
- Exact-copy VOD may also serve fragment ranges from the source. Unverified.

Required: register demand at the foreground source-read layer, or list each excluded path with a reason.

**6. Evidence weighting.**
- The only concurrent reader logged during the 14:06 attempt is the full-source attestation. It resumed 0.66 s before the producer installed.
- The scanner was observed later; its presence at 14:06 is unknown.

Required:
- Lead the summary with the hash.
- Ship the `analysis_source_may_continue` fallback gate first, as a small isolated change.

**7. Supply floor and the cheap lever.**
- Below 1x supply nothing plays: transcode reads the same bytes. Say so.
- Read-ahead 128 KiB→8 MiB gave ~1.9x throughput, but raised per-op latency to 354 ms.
- A daemon-side input prefetcher (large sequential reads / fadvise on the producer input) avoids host tuning. Add it to M1 with measurement.

## P3
- Ship M2 (selected-audio recipe parity) now. It is independent and small.
- Ready path: 673 ms VOD create + 1,741 ms attach→present already exceeds 2 s, before the unaccounted ~1.4 s. M0 ownership is right; both halves must shrink.
- Storage: 29 MiB/s NAS-local is low for a 20 TB drive. Check `filefrag` on the source, and whether the enclosure can use UAS instead of `usb-storage`.
- `rolling_insufficient_capacity` retirement at 0.818x is honest. The alternative, keep publishing with visible rebuffer attribution, is a product decision for the user, not a silent default.

## Keep
- The cold-I/O diagnosis, the scanner A/B/A control, and the explicit measurement limits.
- Whole-file integrity as a precondition for immutable reuse; no partial hash. Not a precondition for rolling media.
- M0 ownership of the ready-path 2 s target. Never reset the latency clock on a successor attempt.
- Confirmed-reap ownership for cleanup fences. A quick refusal never counts as a fast start.
- M4's rate/supply envelope and per-class acceptance with no discarded attempts. No generalising from the single 720p trial.
- No Developer switch for these correctness repairs.
- §2.5 audio-identity defect and M2 as scoped.

## Ledger
- Record this as one Opus 5.5 review plus one independent Fable 5.1 review.
- No runtime test or build was run by the reviewers.
- Each source citation is at ed708eda6.

</details>
