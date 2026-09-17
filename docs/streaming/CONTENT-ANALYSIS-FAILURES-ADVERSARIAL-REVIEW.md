# Content analysis review — retry expiry and recovery identity need correction

**Status:** done; request changes before implementation · **Written:**
2026-09-17 UTC · **Review method:** independent adversarial agent plus
primary-agent source verification and local predicate reproductions.

Review of [the root-cause and proposed-fix document](CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md).
The original proposal is unchanged so Fable can compare it with these
findings. This review does not implement the repair or authorize fleet
changes.

## Verdict — the diagnosis stands; two repair contracts need changes

The source establishes the container-versus-video duration mismatch, and
the Avatar example directly illustrates it. The proposal appropriately
limits its retained-record statistics and does not claim to have established
a current 45% failure rate or independently decoded every affected file.

Two actionable issues prevent treating the proposed repair as ready to
implement. The new timeout schedule conflicts with the existing queue
expiry policy, and preserving existing recovery deduplication behavior
would lose the per-video identity the repair needs.

| Finding | Severity | Proposal location | Required change |
|---|---|---|---|
| R1 — scheduled retries expire before the attempt ceiling | P1 | §4.2, lines 252–256 | Define compatible retry, queue-age and reenqueue accounting |
| R2 — recovery can skip or rebuild the wrong video pipeline | P2 | §5, lines 293–312 | Carry the exact video identity through eligibility and successor creation |

Both findings were confirmed by the independent agent. R1 was first found
by the primary agent; R2 was independently traced through the Store and
worker selection paths after the primary agent identified the eligibility
predicate.

## R1 — the fifth retry cannot survive the six-hour queue expiry

**Trigger:** a file repeatedly exceeds its processing budget. The proposal
adopts waits of 30 minutes, one hour, two hours and four hours while retaining
the configured charged-attempt ceiling.

**Deployed behavior:** both Store implementations define
`QUEUE_ELIGIBILITY_MS = 6 * 60 * 60 * 1000`. Their enqueue/claim maintenance
marks a queued job `queue_expired` whenever `created_at_ms` is more than six
hours old, without exempting a future `not_before_ms`. The default maximum
is five charged attempts.

Ignoring all processing time, the proposed schedule is:

| Attempt | Earliest start after job creation |
|---|---:|
| 1 | 0 hours |
| 2 | 0.5 hours |
| 3 | 1.5 hours |
| 4 | 3.5 hours |
| 5 | 7.5 hours |

The fifth attempt is therefore expired before it becomes eligible. Actual
processing or queue delays only bring expiry closer. Moreover,
`enqueue_cluster_fragment_index` permits reopening `queue_expired` work and
resets its attempts, attempt errors and creation timestamp. If rediscovered
through that path, a persistently slow identity can repeat this cycle
instead of reaching its promised lifetime attempt ceiling.

**Evidence at deployed commit `c9e4edf4`:**

- [SQLite Store](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs):
  `QUEUE_ELIGIBILITY_MS`, line 24; `enqueue_cluster_fragment_index`, lines
  1876–1895 and 1921–1945; `claim_cluster_fragment_index_excluding`, lines
  1997–2014.
- [Hiqlite Store](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs):
  `QUEUE_ELIGIBILITY_MS`, line 29; claim expiry, lines 2788–2810.
- [Shared queue policy](../../crates/plurx-core/src/store/fragment_index_cluster.rs):
  `DEFAULT_ANALYSIS_MAX_ATTEMPTS = 5`.

**Required correction:** specify one compatible policy for intentional
backoff, abandoned-queue expiry and total charged attempts. Either bound the
retry schedule within an explicit total lifetime, or distinguish scheduled
retry age from abandoned unstarted work. Preserve charged failure history
across automatic reenqueue; refreshing creation time alone does not enforce
a lifetime attempt ceiling. State how existing backoff settings interact
with the new timeout-specific schedule.

**Acceptance:** both backends need deterministic-clock tests crossing six
hours during a scheduled backoff, then rediscovering the same failed
identity. Prove the documented number of charged attempts occurs or an
explicit lifetime limit ends it, and that automatic expiry/reenqueue cannot
reset the bound indefinitely.

## R2 — recovery eligibility and successor creation lose video identity

**Trigger:** a Dolby Vision file has a ready stripped pipeline and a failed
converted or preserved pipeline. The proposal promises recovery per
source/pipeline identity but says to preserve existing successor-dedup rules.

**Deployed behavior:** `reopenable_analysis_requests` matches a successor
by file, source size/mtime, component and target node, omitting
`video_identity`. A ready stripped request suppresses a failed converting
request from the recovery candidate list.

Even when a caller selects the failed request directly,
`retry_analysis_request_admin` omits `video_identity` from the successor
INSERT in both backends. The default is the empty string.
`fragment_index_requested_video_options` interprets empty identity as the
first missing local pipeline, falling back to the first/stripped pipeline.
The recovery can rebuild a sibling and leave the intended failure stranded.

**Evidence at deployed commit `c9e4edf4`:**

- [SQLite Store](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs):
  `reopenable_analysis_requests`, lines 1244–1254;
  `retry_analysis_request_admin`, lines 1299–1325.
- [Hiqlite Store](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs):
  eligibility, lines 2060–2069; successor INSERT, lines 2108–2135.
- [Worker selection](../../crates/plurxd/src/state.rs):
  `fragment_index_requested_video_options`, lines 2540–2572.

A local in-memory SQLite reproduction with a failed `convert` request and a
ready `strip` request returned no candidates using the deployed eligibility
predicate. Adding matching `video_identity` to that predicate returned the
failed converting request. This proves the suppression mechanism; it is
not a full Store integration test or a complete proposed SQL fix.

**Required correction:** preserve the deduplication invariant, not the
existing comparator. Define the complete verified source/pipeline/video
identity for recovery; include it in eligibility and carry it into every
successor. Resolve legacy empty identities from the referenced job and
verified provenance, or leave them explicitly unresolved. Do not guess a
pipeline from whichever artifact happens to be absent on the serving node.

**Acceptance:** both backends must cover ready stripped + failed converting,
two different failed video identities, explicit retry retaining its identity,
and ambiguous legacy empty identities. Include a concurrent recovery case
that proves one successor per intended identity without suppressing its
siblings or overwriting a valid artifact.

## Review boundaries — open decisions are not additional hidden findings

The proposal already marks timing authority, missing/conflicting duration,
Matroska tag trust, diagnostic persistence, legacy recovery attribution and
trailing-audio playback policy as unresolved review decisions. Those still
need resolution before implementation, but this review does not inflate the
finding count by restating them as newly discovered defects.

The current process runner preserves a pre-existing shortfall outcome even
when FFmpeg subsequently reports failure. When implementing typed outcomes,
define precedence between a shortfall symptom and an operational process
failure; the proposal already calls for process diagnostics and regression
coverage, so this is a clarification within that open contract, not a third
confirmed finding.

No Rust or production changes were made. No full index build, fleet replay
or Rust suite was run for this document review. Validation consisted of
source inspection, an executable retry-schedule calculation, the isolated
SQLite eligibility reproduction, and documentation checks.

## Reviewed versions

- Proposal SHA-256 before review:
  `e81cac30551af49a47ef984854ef1b548b5ba688cd16ad227ec88d36f22ed557`.
- Deployed source:
  `c9e4edf451e12247a7aa4188903e5ba36888e7e9`.
- Source links open the local checkout; the line numbers above refer to
  `git show c9e4edf4:<path>`, because the working checkout is older.

Close R1 and R2 with explicit proposal amendments, then obtain Fable's
decisions on the remaining alternatives. A successful root-cause diagnosis
does not make the recovery contract safe to execute unchanged.
