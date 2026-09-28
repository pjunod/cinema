# Subtitle cluster sources — contracts to settle before implementation

**Status:** request changes · **Written:** 2026-09-24 · **Reviewer:** Codex
· **Reviewed checkout:** `bafeb0876` · **Reviewed document:**
[SUBTITLE-CLUSTER-EXTRACTION-PLAN.md](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) v1
· **Author's response:** every finding accepted; dispositions in the plan's
§0, contracts in §3.9–§3.11, acceptance cases bound to milestones in §5
(plan v2, same day).

Review of "Subtitle extraction on the cluster — why every first subtitle
waits on one node's full read, and how the pool takes it over", proposal v1
dated 2026-09-24. Section references below refer to that proposal (v1).

This is a revision handoff to the proposal's author. It requests changes to
the design and its acceptance checks; it does not authorize implementation,
deployment, or changes to client behavior. Re-verify source references
against the intended implementation base because line numbers may move.

## 1. Verdict and evidence boundary

**Request changes before implementation.** Producing subtitle artifacts
during an existing demux, distributing them through peer transport, and
backfilling during idle periods is a useful direction. The blocking issues
are the contracts between those pieces: request identity, track coverage,
burn fidelity, scheduling progress, client waiting, and source identity.

The review inspected the current source. It did not run experiments E1–E5,
compile a patch, query the deployed fleet, or verify the reported library
counts and extraction timings. The proposal names deployed commit
`936157b4b`; the inspected checkout was `bafeb0876`. The findings below name
the inspected behavior rather than claiming an independent fleet audit.

The measured 402-second extraction is used as a premise supplied by the
proposal. The client retry constants and queue semantics were checked in
source. No finding asserts that the proposed FFmpeg tee experiment has
already failed.

## 2. Blocking findings

### R1 — Playback priority must not imply a forced rebuild

**Priority:** P1 · **Proposal:** §3.4 and §3.6.

**Evidence.** `analysis_request_generation` in
`crates/plurxd/src/state.rs:2522` generates a fresh UUID when
`force_rebuild` is true. `valid_request` in
`crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs:1154`
requires `force_rebuild == (priority == "forced")`. The active unique index
at that file's line 668 includes `pipeline_version`, `video_identity`, and
`requested_generation`, in addition to the fields quoted in the proposal.
The separate forced-successor indexes cover only `skip_markers` and
`fragment_index`.

**Failure.** Reusing those semantics for a playback miss creates a new
generation rather than joining a stable per-source request. Concurrent
misses can create multiple subtitle jobs. A forced request also cancels
normal work through the existing supersession transaction; it does not
promote the background request in place. This contradicts the promised
single read and the claim that playback and backfill are the same row.

**Required revision.** Separate urgency from rebuild intent. Specify the
stable subtitle request identity and the atomic enqueue/join/priority
promotion operation. Define how a ready request is repaired when its
artifacts disappear: the existing enqueue SQL does not consult subtitle
publications, so losing a publication does not automatically invalidate a
ready tombstone.

**Acceptance.** Three simultaneous playback misses and one existing
backfill request produce one active job. A playback join preserves work
already performed. Removing the last usable artifact allows a new bounded
repair attempt despite the earlier ready row.

### R2 — Publication existence is not complete track coverage

**Priority:** P1 · **Proposal:** §3.3–§3.6.

**Evidence.** Hydration fetches one requested track and republishes locally.
Playback enqueue and backfill eligibility nevertheless test whether any
publication row exists for the file stamp. Existing manifests can contain
only PGS tracks, or settled `malformed` and exhausted `transient` entries.
`Manifest::latched` in `crates/plurxd/src/subtitle_source.rs:208` means all
listed ordinals have settled verdicts; it does not mean every eligible
track has usable bytes.

The current producer publication at
`crates/plurxd/src/subtitle_ride_along.rs:1363` swaps the manifest and then
deletes artifacts named only by its predecessor. Reusing it for partial
hydration requires an explicit merge contract.

**Failure.** Node A produces several tracks, node B hydrates only one, and
A later loses its store. B's publication can prevent extraction of every
missing track. Existing PGS-only publications can similarly suppress text
backfill. Successive single-track hydrations can overwrite each other's
manifest coverage if published as replacements.

**Required revision.** Distinguish extraction coverage from locally
available artifacts. Define per-track verdicts and availability, select
peers by the requested ordinal and representation, and merge hydrated
entries under per-file serialization. Define recovery for incomplete
coverage, absent bytes, and tracks that have settled without a usable
artifact. A missing track must not invalidate unrelated valid tracks in
the same publication.

**Acceptance.** Hydrate two different ordinals concurrently and retain
both. Remove the original complete publisher and request a third ordinal;
the request must recover. An old PGS-only manifest must not count as text
coverage. Exercise malformed, empty, and exhausted-transient verdicts.

### R3 — WebVTT alone cannot fulfill the text-burn promise

**Priority:** P1 · **Proposal:** §3.1–§3.2 and the objective.

**Evidence.** The session-start path calls `ensure_burn_source` in
`crates/plurxd/src/transcode.rs:19375`. Its subtitle-only Matroska preserves
ASS styling, as documented by `ensure_burn_source` in
`crates/plurxd/src/subtitles.rs:1067`. The older text-sidecar selection in
`crates/plurxd/src/transcode.rs:15616` also explicitly excludes ASS/SSA from
WebVTT substitution because conversion discards authored styling and
positioning.

**Failure.** Storing only WebVTT either leaves the original-source burn
extraction necessary or changes the rendered subtitles. Equivalence with
`extract_vtt` verifies the text-display representation, not the original
burn representation.

**Required revision.** Retain a representation sufficient for faithful
burn derivation, or narrow the objective and explicitly retain inline
extraction for affected consumers. Document which representation each
consumer accepts. Place the VTT fast path at a format-aware boundary:
`ensure_vtt_at` is also called for `.mks` burn sidecars, so its name does
not make it a VTT-only function.

**Acceptance.** An ASS fixture with positioning and styling produces the
same burn behavior through the stored-source path as through the original
source. A stored WebVTT track cannot be published under a burn-sidecar key
by the generic cache helper.

### R4 — Enqueue success does not guarantee progress

**Priority:** P1 · **Proposal:** §3.4–§3.5.

**Evidence.** `resolve_analysis_requests` in
`crates/plurxd/src/state.rs:7563` refuses claims when
`pretranscode_worker_idle()` is false. Foreground preemption can produce a
retry without charging an attempt. The proposal suppresses inline work
after enqueue succeeds and releases it only for disabled queueing,
enqueue failure, or terminal failure.

**Failure.** If every eligible worker remains busy, a successfully queued
request can remain unclaimed indefinitely. Repeated uncharged preemption
need not reach the attempt limit. The preference list also lacks an
outcome for publications that exist but have no reachable serving peer.
Cancellation needs a defined outcome as well: it is not necessarily one
of the named terminal failures.

**Required revision.** Specify bounded waiting and recovery for unclaimed
jobs, repeated preemption, cancellation, and unavailable publishers.
Distinguish playback-required extraction from optional backfill. If
fallback extraction is allowed, define its ownership so releasing a
waiter does not silently create duplicate producers. Explain how explicit
operator cancellation remains respected across subsequent client polls.

**Acceptance.** Hold all workers busy, repeatedly preempt a job, cancel a
job, and make every publisher unreachable. Each case must reach the
documented bounded outcome, without indefinite pending responses or a
storm of independent extractions.

### R5 — Existing startup retries cannot cover a cold extraction

**Priority:** P1 · **Proposal:** §3.8 and §6.2.

**Evidence.** `CREATE_RETRY` in
`crates/plurxd/src/web/playback-policy.js:868` specifies three retries with
1,000/2,000/4,000 ms backoff and a 60,000 ms absolute deadline. Apple and
Android restate the same policy. Four calls that each spend the proposed
five-second join budget exhaust the ladder in approximately 27 seconds,
before other request overhead. The absolute upper bound remains 60 seconds.

**Failure.** A 402-second extraction cannot become visible on the next
automatic startup retry: the client has already exhausted its ladder and
stopped. Moving the full read to a peer does not change that duration.

**Required revision.** Either revise the preparation/retry contract or
state that a cold extraction requires a later manual retry. Separate the
acceptance for already-produced artifacts from the acceptance for cold
preparation. If no client changes remain a guardrail, remove the promise
of automatic completion after minutes of queued work.

**Acceptance.** A deliberately slow extraction exceeding 60 seconds must
produce exactly the documented client outcome. Verify this separately
from a warm peer-hydration case that fits the existing startup budget.

### R6 — The direct text endpoint has no existing pending response

**Priority:** P1 · **Proposal:** §3.4 and §4.

**Evidence.** The text subtitle endpoint in
`crates/plurxd/src/http/stream.rs:2496` awaits `ensure_vtt_bytes` and maps
an error to HTTP 500. It does not implement the startup 503 or HLS empty
segment contract used by other consumers.

**Failure.** Returning a queued/pending error from the shared subtitle
helper makes this endpoint return an internal error. Assuming that its
caller polls like an HLS rendition is insufficient. Offline packaging
also awaits a completed VTT and needs a defined queue-join behavior.

**Required revision.** State the completion and pending behavior for each
consumer: session-start burn, HLS rendition, direct text, PGS overlay, and
offline packaging. Preserve existing behavior where feasible; identify
explicitly any HTTP or client contract that must change. Reconcile the
blanket "no handler awaits an extraction" guardrail with the existing
direct endpoint and the proposed queue semantics.

**Acceptance.** Exercise a direct text request and an offline job against
a cold queued source. They must complete or return their specified
recoverable outcome; generic HTTP 500 is not a pending protocol.

### R7 — Peer hydration needs portable source identity

**Priority:** P1 · **Proposal:** §3.3.

**Evidence.** `Manifest::source_matches` in
`crates/plurxd/src/subtitle_source.rs:190` requires matching device and
inode for the burn consumer when the live source has them. These are local
filesystem identities and need not match between nodes serving the same
media. The proposal keys publications by file ID, size, and mtime and says
hydration publishes as the producer would, without defining how the
source stamp changes.

**Failure.** Copying the producer's stamp can make a valid peer artifact
unusable for burn on the receiver. Replacing it with the receiver's stamp
without a validation rule would discard the identity guarantee rather
than implement it.

**Required revision.** Separate portable source identity from the
receiver's held-file identity. Define how the receiver validates the
artifact's source, binds it to its own held file, and fences publication
against replacement during hydration. An artifact checksum proves the
transferred bytes, not which local source those bytes belong to.

**Acceptance.** Hydrate between two nodes whose source files have
different device/inode values. Burn must use the verified artifact.
Replace the receiver's source during hydration and verify that the
artifact cannot be accepted for the wrong held file.

## 3. Scope claims to correct

**"Once per file per cluster" needs a qualified ownership rule.**
Deduplicating the new analysis component does not coordinate it with an
index pass already producing the same subtitles, or with an index pass
on another node. Specify coordination between those producers, or promise
best-effort reuse and bounded duplication rather than exactly one read.
Eviction, source changes, and recovery also legitimately require rereads.

**Keeping the window owner unchanged preserves source reads.** The HLS
path can still start a window extraction while the whole track is absent.
That can happen while a queued cluster job or peer hydration is pending.
Distinguish eliminating full-track inline reads from eliminating all
source reads on the serving node. If the latter remains the objective,
window admission must account for cluster preparation.

**The overnight acceptance contradicts peer hydration.** The final §6.2
check says "no hydration runs" while allowing a peer to answer. Backfill
places artifacts on the producing node; a different serving node can
still need hydration. The meaningful assertion is that no new full-source
extraction runs and the artifact arrives within the chosen client budget.

## 4. Requested author response

Return a revised proposal with a disposition for R1–R7: accepted, rejected
with source evidence, or deferred with an explicit reduction in scope.
Keep the finding IDs so the next review can identify what changed.

The revision should contain three concrete contracts:

1. A request lifecycle covering enqueue, join, priority promotion, claim,
   preemption, cancellation, completion, artifact loss, and repair.
2. A publication model distinguishing extraction coverage, per-track
   verdict, locally available representations, portable source identity,
   and partial hydration merge behavior.
3. A consumer table naming the required representation, pending response,
   wait owner, retry budget, and fallback condition for each consumer.

Retain E1–E5 as empirical producer checks. Add the focused acceptance cases
under each finding to the appropriate milestone. The experiments can
establish FFmpeg behavior; they cannot substitute for the queue, identity,
and client contracts above.
