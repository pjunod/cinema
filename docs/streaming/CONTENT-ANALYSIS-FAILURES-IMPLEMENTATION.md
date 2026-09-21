# Content analysis repair — build contract for completion checks and recoverable failures

**Status:** open — implementation handoff; no implementation or deployment
claimed · **Written:** 2026-09-17 UTC.
**Executes:** the [root-cause diagnosis](CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md)
and findings R1/R2 from the
[adversarial review](CONTENT-ANALYSIS-FAILURES-ADVERSARIAL-REVIEW.md).
**Verified source base:** `363a22e28aa53094d899a9ad3c812241a6243548`.

Implement the work packages in §10. This document supersedes the original
proposal's open design alternatives for this implementation; it does not
claim Fable has approved them. Preserve the RCA and review as evidence.
Reconcile every source seam against current intended `main` before editing:
the documentation checkout is older and has unrelated work in progress.

## 1. Required result and scope

A complete selected-video pass must not fail because audio or subtitles
extend the container duration. Actual partial video output must still fail.
Operational timeouts must carry an accurate cause, bounded retry state and
useful progress. Recovery must reach the exact failed video pipeline once,
preserve successful siblings, and leave an auditable history.

The observed examples are file 9 (reference film K), 3451 and 3498 (reference episode L), and 3634
(Family Guy). Their indexed video coverage agrees with video metadata while
container duration is longer. File 120 (reference film G) supplies timeout evidence,
but its later success was not correlated to the same pipeline/source version;
do not make that stronger claim in release notes or tests.

**Non-goals:** replace media, change NAS settings, broadly rescan libraries,
raise worker concurrency, alter video bitstream filters, change playback's
stream selector, discard successful indexes, or repair unrelated historical
`attempt_limit` and `queue_expired` records. Do not widen duration tolerance
to hide short reads. No native client changes are required unless an actual
existing strict decoder of the changed analysis response is found.

### 1.1 Decisions resolving the proposal and review

| Issue | Build decision | Reason |
|---|---|---|
| Completion duration | Resolve timing for exactly the mapped video stream; retain container duration separately | A video-only pipe cannot cover a longer audio track |
| Missing or conflicting timing | One bounded held-source probe; if still unresolved, fail as `index_completion_unverified` before a whole-file pass | Clean EOF alone does not establish that a prefix is the complete source |
| Existing successful indexes | Keep their bytes, version and fingerprints | The acceptance predicate changes; emitted media does not |
| R1: backoff versus expiry | Mark index retry cycles explicitly; exempt them from the six-hour unstarted-queue expiry and impose a seven-day deadline plus the existing attempt ceiling | Intentional backoff must survive without creating an unlimited retry loop |
| R2: recovery identity | Match and copy the exact video identity; resolve legacy blanks from job provenance | Ready stripped video is not ready converted video |
| Diagnostics | Bounded versioned record stored with the fenced job transition, mirrored locally | Any node serving the admin page must see the cause |
| Recovery rollout | Explicit preview and exact candidate application, with durable per-repair receipts | Repeated or concurrent actions must not create another recovery cycle |
| Playback tail | Retain current segment-plan tail behavior and test it with the repaired indexes | Correcting the index must not silently truncate audio or invent video |

## 2. Starting points and ownership

The index builder, state orchestration, analysis HTTP handler and the three
queue Store source files were compared between deployed `c9e4edf4` and the
verified base above; those files were unchanged. Other shared helpers must
still be reconciled at implementation time.

| Concern | Existing owner and entry points |
|---|---|
| Index argv | [Core transcode](../../crates/plurx-core/src/transcode/mod.rs): `copy_index_pipe_args_with_input`, currently `-map 0:v:0? -an -sn` |
| Pipe and completion | [fragindex.rs](../../crates/plurxd/src/fragindex.rs): `IndexPass`, `build_with_args`, `index_stream_with_progress` |
| Selected-stream facts | [decode_facts.rs](../../crates/plurxd/src/decode_facts.rs): existing stream ordinal resolution; FFmpeg counts attached pictures in `v:0` |
| Held-source access | [fragment_index_cluster.rs](../../crates/plurxd/src/fragment_index_cluster.rs): attestation and file handles; [ffmpeg.rs](../../crates/plurxd/src/ffmpeg.rs): held-source probing and bounded child output |
| Scheduling and result policy | [state.rs](../../crates/plurxd/src/state.rs): `index_file_budget`, `fragment_index_requested_video_options`, local and cluster index workers |
| Shared contracts | [segplan.rs](../../crates/plurx-core/src/segplan.rs): `IndexRefusal`, `FragmentIndexOutcome`; [queue Store API](../../crates/plurx-core/src/store/fragment_index_cluster.rs): job/request identity and retry constants |
| Local refusal store | [store/fragindex.rs](../../crates/plurx-core/src/store/fragindex.rs): `record_outcome`, `outcome`, `next_attempt_at_ms` |
| Durable queue transitions | [SQLite implementation](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs) and [Hiqlite implementation](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs) |
| Admin control and presentation | [http/analysis.rs](../../crates/plurxd/src/http/analysis.rs), [http/browse.rs](../../crates/plurxd/src/http/browse.rs), [web/index.html](../../crates/plurxd/src/web/index.html) |
| End-of-title behavior | [vodserve.rs](../../crates/plurxd/src/vodserve.rs): `track_durations`; [segplan.rs](../../crates/plurx-core/src/segplan.rs): `append_audio_tail` |

Extract the new pure completion and failure-policy helpers into small focused
modules where useful. Keep queue authority in Store transactions and child
ownership in the existing process-control path. Do not build a second job
scheduler, recovery service or source-attestation scheme.

## 3. Completion expectation — facts, arithmetic and source binding

### 3.1 Add an explicit expectation to the index recipe

The following is a **new interface sketch**, not an existing API. Equivalent
names are acceptable; every field and invariant is required.

```rust
struct VideoCompletionExpectation {
    stream_index: u32,              // absolute source stream index
    duration_num: u64,              // seconds, rational numerator
    duration_den: u64,              // positive rational denominator
    provenance: CompletionProvenance,
    source_object_version: String,  // existing held-source version token
}

enum CompletionProvenance {
    StreamTicks,
    StreamSeconds,
    MatroskaDurationTag,
}
```

Carry this value in `IndexPass`, or a companion recipe object constructed
with the same selected stream and held source. Make the production builder
require a resolved expectation. A test-only raw stream helper may still
accept unknown duration to exercise parsing, but no production caller may
use it to bypass completion validation.

Keep `MediaFile.duration_ms` unchanged. It remains the budget input and the
existing playback/container runtime; it must no longer be passed as the
video-completion lower bound. Diagnostics can show both values.

### 3.2 Resolve exactly the video FFmpeg maps

Use the same stream ordering/ordinal semantics as the actual index argv.
At this base, `0:v:0?` is the first video stream including an attached
picture. Resolve that stream's absolute index. Do not skip cover art only
in the expectation resolver. If the selected stream is an attached picture,
return `unsupported` with an accurate detail; changing the map to another
stream requires a separate shared index/playback decision.

For the selected stream, collect these candidate durations:

1. Positive integral `duration_ts` times positive rational `time_base`.
2. Positive finite `duration` parsed as decimal seconds.
3. Matroska `DURATION`, then `DURATION-eng`, strictly parsed as
   `HH:MM:SS[.fraction]`; minutes and seconds must be below 60.

Use checked integer/rational arithmetic, not floating-point multiplication.
Reject negative, zero, nonfinite, malformed or overflowing values. Hours
may exceed 23. Bound parsing to signed 64-bit milliseconds of duration and
use checked wider intermediates for conversion/comparison. Reject oversized
input strings before parsing; 64 bytes per numeric duration is sufficient.

When multiple valid candidates differ by more than 2,000 ms, classify the
metadata as conflicting. Otherwise choose the first available candidate in
the order above. An invalid tag does not override a valid stream duration,
but include a bounded diagnostic flag. Never consult another video, an
audio/subtitle duration or the format duration to fill a missing candidate.

### 3.3 Obtain current evidence without another whole-file scan

The scanner's stored JSON is useful for inventory and legacy explanation,
but size/mtime alone does not prove that its timing describes the current
held object. For each new build, obtain one fresh bounded metadata probe
against the already-attested handle, or reuse a fresh probe already captured
for that exact object during the same attempt. Do not reopen the pathname
on Unix. Preserve Windows' existing pre/post handle/path checks.

Use a dedicated indexing probe budget of 30 seconds and an output cap of
1 MiB, within the existing overall per-file build budget. Reuse the process
and descriptor helpers; do not globally increase `bounded_command_output`'s
timeout, since playback preparation has a separate contract. Reset or
position independent held-source reads as the existing helper requires so
probing cannot leave the actual index input at a nonzero offset.

Recheck the held source's object version after probing and before
publication. A mismatch follows the existing source-changed path; do not
cache the expectation under the old source or classify it as short video.
Source attestation remains the existing sampled scheme, not a claim of
whole-file cryptographic verification.

If the probe times out, fails to read a currently valid handle, or cannot
spawn due to a classified transient resource error, return the relevant
operational cause in §4. Missing executables or unsupported input do not
receive automatic repeated whole-file attempts. If the probe succeeds but
timing remains unknown/conflicting, return `index_completion_unverified`
without launching the long index pass. Do not loop probes in one attempt.

**Compatibility check:** before enabling the new predicate, inventory
selected-video timing in a bounded sample of currently successful indexes,
not just the failing cohort. If valid files that the old builder accepted
would become unverified, record their count and representative metadata.
Do not enable a broad regression silently; provide an explicit bounded
verification extension or record that rollout is blocked on those cases.
Existing positive artifacts remain usable during this investigation.

### 3.4 Apply the selected-video lower bound

Let `covered` be the sum of video sample durations and `scale` the output
timebase. With expectation `num / den` seconds, reject short coverage when:

```text
(covered + 2 * scale) * den < num * scale
```

Use checked wide intermediates. Arithmetic overflow is unverified metadata,
not a successful comparison. The exact two-second boundary passes.
Only presentation output that otherwise satisfies existing index invariants
may reach this check.

Keep durations as rational values through validation; round down to
milliseconds only for display. Detect output decode-timeline gaps/overlaps
that make summed sample durations differ from the normalized track span by
more than two seconds and return `index_completion_unverified`. Normalize
an initial DTS offset by subtracting the first DTS, not by extending video
to the container runtime. Tests must show how nonzero starts, edit lists
and VFR map through the unchanged production argv; do not silently rewrite
fragment timestamps or counts in this repair.

This is a completion lower bound, not proof that a container's own metadata
is infallible. Retain clean parser completion, process-exit success, source
fencing and all existing codec/promotion invariants. A shortened prefix with
the original video expectation must fail. A source that consistently lies
about its own duration is outside this metadata-based integrity guarantee.

## 4. Failure classification — preserve cause before choosing disposition

### 4.1 Separate parser observations from final process outcome

Refactor the stream reader to return candidate index/observations or parser
failure. The runner combines them with deadline state, source state and
child exit before choosing a final typed cause. The current pattern that
preserves `Truncated` even when the child exits nonzero must not survive.

Apply this precedence:

1. Lease loss, user cancellation or playback preemption: existing yield/
   cancel behavior; no new content refusal and no charged content retry.
2. Source object changed: existing source-identity refusal; never publish.
3. A deadline actually fired: timeout cause, retaining progress and the
   subsequent kill/exit as secondary diagnostics, not a source corruption.
4. Spawn/read/process failure: typed operational or unsupported cause.
   A shorter output is a symptom here, not a terminal video-shortfall proof.
5. Successful exit but malformed/unfinished fragment stream: output failure.
6. Successful process and parser but unknown timing or excessive coverage
   shortfall: the corresponding completion cause.
7. All checks pass: publish through the existing fenced artifact path.

Drain stderr concurrently with a bounded rolling buffer. Cap the tail at
16 lines and 4 KiB **while reading**, including a single enormous line;
truncating a buffer after collecting unbounded output is not sufficient.
Bound the full serialized diagnostic record to 8 KiB. Store no credentials,
source paths, raw argv or arbitrary probe documents in that record.

On timeout/cancellation, terminate and reap the job-owned child using the
existing platform process-control abstraction. Use the existing five-second
exit grace as an upper bound; abort/join the stderr task so it cannot leak.
Keep last progress outside the timed reader future. Call it output bytes
read if it measures the pipe; do not represent it as source bytes scanned.

### 4.2 Stable codes and shared policy

These new codes are required spellings. Existing source, lease, cancellation
and unsupported codes retain their meanings.

| Code | Automatic retry | Required interpretation |
|---|---|---|
| `index_budget_exceeded` | Yes, bounded by §5 | Whole attempt exceeded its per-file budget |
| `index_probe_timeout` | Yes, bounded by §5 | Selected-video metadata could not be obtained in time |
| `index_source_io` | Only classified transient I/O | Read failure, not proof of media damage |
| `index_process_failed` | Only classified transient resource/process failure | Include exit/status category; unknown nonzero failures are terminal |
| `index_video_shortfall` | No for unchanged source | Successful process/parser, authoritative video expectation not covered |
| `index_completion_unverified` | No automatic loop | Metadata or timeline semantics cannot establish completeness |
| `index_output_malformed` | No when process exited successfully | Invalid/partial output structure |
| `index_retry_window_expired` | No | Seven-day retry cycle ended before success |

Use typed errors from I/O and process helpers wherever available. Restrict
automatic retry to an explicit tested allowlist (for example interrupted or
timed-out I/O and temporary process resource exhaustion); arbitrary stderr
substring matching must not turn an unknown error into an endless retry.
Retain `attempt_limit` when the configured charged-attempt ceiling is reached,
but retain the underlying failure in diagnostics and attempt history.

One shared pure policy produces `retryable`, next-attempt time and terminal
reason for both local and cluster workers. Update browse summaries and every
`is_due`/retryability consumer; a new code must not become retryable merely
because its legacy coarse category was `truncated`.

## 5. Retry lifecycle — close adversarial finding R1

### 5.1 Explicit bounded index retry cycles

Keep the current per-file budget formula and the existing configured maximum
charged attempts (default 5, hard maximum 20). For retryable failures in §4,
use this cause-specific delay after charged attempt `n`, including failures
before the first index retry:

```text
delay_ms(n) = min(30 minutes * 2^(max(n, 1) - 1), 24 hours)
```

Use checked/saturating arithmetic. These constants belong to the shared
index-failure policy; generic analysis backoff settings continue to control
other failure types and do not change these delays. Document that distinction
in the existing operations/settings reference. No global configuration
default changes are part of this repair.

Persist `index_retry_deadline_ms` on the first retryable typed index failure:
`now + 7 days`. Zero means no new-policy index cycle has started. Once set,
the deadline never moves during that generation, even if later attempts
fail for another reason. A new explicitly authorized generation has its own
attempt budget/deadline; automatic rediscovery does not create one.

Failure transition order is: enforce source/fence, record the cause, enforce
configured attempt ceiling, enforce the retry deadline, then schedule.
If the next retry would be at or beyond the deadline, end with
`index_retry_window_expired` immediately. Do not shorten the delay merely
to fit in one last attempt. Claims and expired-lease recovery also enforce
the same fixed deadline. At the deadline, revoke a running claim through
the normal fence/cancellation path; it must not publish afterward.

### 5.2 Queue expiry must not erase charged failures

Change **every** enqueue/claim/rebind maintenance path in both Store
implementations, not just `fail_cluster_fragment_index`:

- A new-policy index cycle with a nonzero deadline is exempt from the
  six-hour `created_at_ms` expiry while within its deadline. This includes
  `queued` rows whose `not_before_ms` is still in the future.
- At/after its deadline it ends as `index_retry_window_expired`; that code
  is not eligible for automatic reopen or discovery reset.
- When existing `queue_expired` work is reopened automatically, never reset
  a positive charged-attempt count or its history/deadline. Unattempted rows
  may retain the old requeue behavior. A row at its attempt ceiling stays
  terminal until an explicit administrator or one-time repair generation.
- A foreground priority boost cannot pull a scheduled new-policy retry
  earlier than `not_before_ms`. Repeated playback requests must not bypass
  the backoff by hitting the existing `MIN(not_before_ms, incoming)` upsert.
- Preemption still refunds the current claim as today; it does not clear
  previously charged failures, extend the deadline or generate new history.

Do not reset `created_at_ms` on every retry to solve R1. It would conceal
queue age and permit unbounded reenqueue cycles. The separate deadline is
the reason the longer schedule can coexist with abandoned-queue cleanup.

Mirror the charged-attempt ceiling and fixed retry deadline for the
standalone local index worker. Local records are explanatory in cluster
mode; the replicated job controls the retry clock. Do not impose a second
independent node-local delay on a valid cluster claim.

## 6. Persistence and APIs — bounded facts with atomic state changes

### 6.1 Additive migrations and compatibility

Allocate migration versions from the intended implementation base; do not
reuse a number from this document. Both SQLite and Hiqlite require the same
logical additions. Extend telemetry/sidecar migration separately.

Required job additions (SQL shape is normative; version placement is not):

```sql
ALTER TABLE cluster_fragment_index_jobs
  ADD COLUMN index_retry_deadline_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE cluster_fragment_index_jobs
  ADD COLUMN index_diagnostic_json TEXT NOT NULL DEFAULT '';
```

The diagnostic JSON is a version-1 object with code, retryable disposition,
claim fence, attempt number, selected stream, expectation provenance,
covered/expected/container milliseconds, fragment count, output bytes,
elapsed/budget milliseconds, exit category, safe stderr tail and recorded
time. Missing values are null, not fabricated zeroes. Invalid/unknown JSON
versions degrade to an unavailable diagnostic in reads; legacy jobs with
empty JSON remain readable. Bound numeric values and serialized size at the
Store boundary, not only the HTTP boundary.

Persist a diagnostic and its queue transition in the **same fenced Store
transaction**, conditional on running owner/fence/unexpired lease/current
source. If the fence fails, write neither. For a terminal deadline sweep,
preserve the last failure diagnostic and append the terminal lifecycle code;
do not invent parser progress. The existing comma-separated `attempt_errors`
remains bounded code history, never a JSON or stderr transport.

Add typed detail, effective retryability, retry deadline and policy revision
to the local refusal record. Retain legacy `truncated`/`unsupported` rows
for backwards reading; either extend their CHECK constraint through the
repository's table-rebuild convention or keep the coarse outcome plus new
typed columns. Choose the latter for this change to avoid rewriting legacy
outcomes. New readers must use the typed policy when present. A local
success still clears only its own negative outcome.

Do not put a new code into a lifecycle counter CHECK constraint without a
migration. Use its existing `other` bucket for aggregate counters in this
repair while preserving exact codes in jobs, bounded history and diagnostics.
Do not add file IDs or arbitrary messages as metric labels.

### 6.2 Durable recovery receipt

Add an `analysis_index_repairs` table on both durable Store backends. It must
have a unique key over:

```text
(repair_revision, file_id, source_size, source_mtime,
 source_sha256, pipeline_sha256, target_node_id)
```

Required additional fields are `video_identity`, predecessor job/cache key,
predecessor fence, successor request ID and creation time. Bound strings as
their existing source/job equivalents are bounded. The only initially
accepted revision is `video-completion-v1`. The stored source digest uses
the existing attestation regime; it is not a new full-file hash.

Insert a receipt and its fully specified successor atomically. A crash can
leave neither or both, never a consumed repair with no successor. Repeated
calls return the same successor. Retain receipts after terminal completion
while the same source identity remains catalogued, even if request-history
cleanup runs. Otherwise pruning history would permit the repair to repeat.
Source deletion may remove its receipts through the existing deletion
contract. A new source/engine pipeline naturally has a different key.

### 6.3 Keep the Store seam typed

Introduce typed inputs/results for the fenced index failure transition and
the preview/apply recovery operations. Do not pass raw JSON SQL fragments
from HTTP. Extend existing Store operations where that preserves their
semantics; add dedicated repair methods where the old admin-force contract
is too broad.

The failure input includes the held job fence, typed diagnostic, next retry
time/deadline and source identity. The Store recomputes/validates permitted
state and attempt policy; a caller cannot request an unlimited deadline.
Recovery candidates include exact identity, observed predecessor fence/
updated time and a concrete eligibility result. Store remains responsible
for atomic source, successor, receipt and headroom checks.

## 7. Recovery — close adversarial finding R2

### 7.1 Define identity before choosing a candidate

Distinguish the three existing values:

- `video_identity` is the copy-video argv fingerprint.
- `pipeline_version` is the engine digest on an analysis request.
- `pipeline_sha256` combines engine, segment-plan version and video argv
  using `cluster_fragment_index_pipeline_digest`.

They are not interchangeable. Define recovery's logical identity as the
existing file/source/digest/pipeline identity plus target scope. Compare the
exact `video_identity` and engine version in request-level dedup predicates.
Do not treat a ready stripped request as a successor of failed converting
work, or a ready old-engine request as current work.

Fix both `reopenable_analysis_requests` and
`retry_analysis_request_admin`: carry `video_identity` in every INSERT and
use identity-aware active/ready exclusion. Apply the same verified identity
rules to generic explicit retry; do not leave the single-row action capable
of quietly changing pipelines. Leave skip-marker identity behavior intact.

Audit the supporting unique indexes and all enqueue/cancellation predicates,
not just these two methods. At the verified base,
`analysis_requests_one_active_forced_successor` is unique only by file,
source size/mtime and component. It prevents two different video pipelines
from having forced successors simultaneously. Some background suppression
predicates likewise treat any forced sibling as covering the whole file.

Migrate that index into separate partial indexes: retain the old semantic
constraint for `skip_markers`; for `fragment_index`, enforce one active
forced successor per file/source/engine/video identity across target nodes.
Carry `video_identity` into the ordinary active-source uniqueness key as
well. An existing active legacy blank-identity request blocks new forced
work for that source until it resolves or is explicitly cancelled, since
it cannot safely be assigned to an arbitrary sibling. Do not auto-cancel it.
Update background suppression and forced cancellation to use the same
identity comparison. Retain current publication head CAS checks in addition
to these indexes; a unique request constraint is not a publication fence.

For a legacy empty video identity, use its referenced job's source and
pipeline digest, enumerate recipes for the current source/engine, and
require exactly one matching recipe. Cross-check that the job belongs to
the request's source/target. Zero matches means superseded; multiple or
missing provenance means `identity_unresolved`. Report and skip those
candidates. Never infer the recipe from the first missing local artifact.

### 7.2 Candidate selection and transaction checks

The repair selects terminal `truncated` index jobs for current sources,
including jobs without operator requests. Resolve any associated requests
by exact job identity. Deduplicate duplicate historical rows into one
candidate per receipt key; separate video identities remain separate.
Do not sweep unrelated error categories or cancelled work into the repair.

Exclude candidates with any valid current artifact for the intended logical
identity, including a current generation reached through the heads table.
An artifact catalog row alone is not byte availability: use existing holder/
hydration verification, and report unavailable artifacts separately instead
of force-overwriting them. Also exclude a matching active/ready successor
and an existing receipt. Scope active target checks consistently with the
existing shared-artifact model; global work must not race a target-specific
worker building the same logical artifact.

On apply, atomically recheck source size/mtime, recorded attestation identity,
engine/video identity, predecessor state/fence, heads, active successors,
receipt and queue capacity. Fresh worker attestation must still match the
expected source digest before building. If any condition changed, return a
specific skipped result; do not recover a replacement file under old evidence.

Create an explicit requested generation with the resolved `video_identity`
and the expected predecessor head. Reuse the existing generation-key and
head compare-and-swap path; do not overwrite a ready artifact or manually
change a failed job to ready. A concurrent valid publication wins. Losing
recovery work settles as superseded/already-ready through normal fences.

One receipt permits one recovery generation, which then has the normal
bounded attempt cycle. A failed recovery does not make the original legacy
row eligible again. An administrator can explicitly create a new retry
generation later; that is a separate action and not automatic reconciliation.

### 7.3 Preview and exact application

Extend `POST /api/v1/analysis/reopen` with an optional, validated
`repair_revision: "video-completion-v1"`. Omitting it keeps generic behavior
apart from the necessary identity correctness fixes. Repair mode requires
component `fragment_index`; reject conflicting components and unknown
revision strings. Authentication remains administrator-only.

Preview is the default and changes no rows. Default limit is 50 distinct
files, maximum 500, plus a hard maximum of 1,500 pipeline candidates. Use a
bounded indexed scan with an explicit continuation cursor; do not scan the
whole library to fill one page. Return `scan_truncated` when appropriate.

Preview returns exact candidate descriptors: job/cache key, target,
observed fence/update time, source identity, video identity, cause if known
and repair revision, plus a stable candidate ID derived from those values.
It also returns file/identity counts, existing receipt counts and skipped
counts by reason. No media reads or attestation refresh is started merely
to preview; candidates requiring that verification say so.

Apply requires the explicit candidate descriptors from preview and
`dry_run: false`; do not silently choose a different batch from a fresh
query. Treat all supplied fields as untrusted and reconstruct/revalidate
them in Store. Reject oversize requests before processing. A stale candidate
is skipped, not substituted. Return created, already-created, already-ready,
source-changed, pipeline-superseded, identity-unresolved and headroom-blocked
results. Preserve 512 slots of active-request headroom transactionally;
simultaneous operators must not each consume the same observed headroom.

Use the same repair scope to bypass a matching legacy local refusal only
for its explicit generation. Cluster claims already own the schedule;
stale local backoff cannot veto them. Do not delete all failures for a file.
When a local-only worker supports explicit repair, persist the same once-per-
identity receipt on its SQLite Store and reset only the selected negative
cycle. Do not automatically reopen legacy local history on startup.

## 8. Operator visibility and playback preservation

The analysis response gains an optional `index_diagnostic` object and an
effective retry time/terminal reason. Read durable diagnostics from the job
when present, including standalone jobs. Node-local diagnostics may enrich
local browse detail but cannot override a replicated retry disposition.
Old clients that ignore new fields retain existing state/code fields.

Show timeouts with elapsed/budget, progress and next retry or terminal limit.
Show a video shortfall with expected and covered video duration plus timing
provenance. Show unverified timing as unavailable evidence, not media damage.
For legacy `truncated` with no reliable detail, state that the original
record did not identify the cause. Remove replacement advice from that
generic category. Render every diagnostic as escaped text.

Update browse/item index summaries as well as the analysis page so one
surface does not say “retry pending” while another says terminal. Keep
historical failures visible. Use labelled historical totals and an explicitly
bounded recent-attempt cohort for effectiveness reporting; do not divide
all old failures by all jobs and call it today's failure rate.

At the verified base, `vodserve::track_durations` uses actual index ticks for
video and container duration for audio. `segplan::append_audio_tail` adds
tail entries when the excess exceeds 50 ms. Preserve this policy in this
repair, including its existing use of container duration as the audio proxy.
Do not change the catalog duration to suppress the tail.

Test materialization and seeking with selected audio that both does and
does not extend past video, including a secondary longer audio track.
Use the existing `a_seek_into_the_audio_tail_spawns_at_the_last_video_entry_and_serves_the_tail`
regression as a starting point. If a repaired index exposes an existing
unmaterializable tail for a real affected title, record that as a release
blocker for that title and supply a narrowly reviewed tail fix; accepting a
valid video index alone is not sufficient playback qualification.

## 9. Invariants and regression matrix

Every new test must prove an observable failure mode, not merely mirror a
helper's implementation. Name focused new tests with `content_analysis_`
so the commands in §11 select them in both backends.

| Area | Cases | Required result |
|---|---|---|
| Wrong duration | reference film K-shaped MP4; MKV with longer audio; subtitle/container tail | Complete video succeeds while unchanged old completion predicate fails |
| True truncation | Same fixture with original video expectation and removed tail; clean-looking prefix | No artifact is published |
| Metadata | Stream ticks/seconds/tags; malformed, missing, contradictory, overflow values | Deterministic provenance or typed unverified result; no container fallback |
| Mapping | Cover-art-first, multiple video streams, absolute stream indices | Validator describes exactly the emitted stream or refuses it |
| Timeline | Exact 2 s boundary, nonzero DTS, edit lists, VFR, gaps/overlaps | Checked comparable timing; no false success from overflow or normalization |
| Child outcome | Nonzero exit after prefix, timeout after progress, missing executable, enormous stderr line | Correct cause precedence, bounded memory, child/task reaped |
| Source/fence | Path replacement, same-size changed object, source update, lease loss after full read | No publication or stale diagnostic under the old fence |
| Retry | Crossing 6 h; default 5 attempts; maximum 20; 7-day cutoff; process restart | Scheduled retries survive; deadline/history do not reset; finite terminal result |
| Queue upserts | Rediscovery, queue_expired reopen, foreground boost, preemption refund | No attempt reset or backoff bypass; old charged errors survive |
| Identity recovery | Ready stripped + failed converted; two failed siblings; blank legacy identity | Correct independent candidates and exactly matching successors |
| Identity indexes | Two explicit forced video siblings; same identity across targets; active legacy blank | Different siblings coexist; duplicate logical work and ambiguous legacy overlap are blocked |
| Idempotence | Repeated apply, concurrent operators, crash before/after transaction, history pruning | One receipt and one successor per identity/revision |
| Races | Source/head changes after preview; ready publication races recovery; stale owner | Specific skip/superseded result without damage to valid artifacts |
| Capacity | 50/500 file limits, 1,500 identity cap, cursor, simultaneous headroom checks | Bounded selection and preserved 512-slot headroom |
| Storage | Upgrade populated legacy DB/sidecar, restart, unknown diagnostic JSON version | History/artifacts retained; both backends agree |
| UI/API | New causes, attempt_limit underlying cause, legacy unknown cause, hostile text | Accurate state, escaped bounded details, no source-replacement guess |
| Playback | Ordinary and DV strip/preserve/convert; start/seek/tail completion | Same index bytes/fingerprint for same pipe; valid tail behavior |

Use at least one deterministic FFmpeg integration fixture with video shorter
than secondary audio. Parser-only fixtures cannot prove stream selection
or actual muxed timing. Cover both SQLite and real Hiqlite Store transactions
for expiry/recovery; string searches over SQL do not prove the race behavior.

## 10. Work packages — each must leave reviewable evidence

### W0 — reproduce and reconcile the base

Create an isolated effort checkout from current intended `main`, retaining
the unrelated working tree. Capture baseline failing/passing expectations,
actual queue transition locations and migration versions. Establish the
pinned compiler loop. Inventory a bounded sample of successful-file timing
for §3.3 and record unknown/conflicting cases. This is read-only production
inspection if live evidence is needed, not authorization to queue work.

**Exit evidence:** failing reference film K-shaped integration fixture on old code;
R1 deterministic-clock reproduction; R2 both-backend candidate/successor
reproduction; base SHA and selected compatibility cohort recorded.

### W1 — shared types and persistence

Implement typed diagnostics, shared retry policy and additive migrations.
Wire fenced diagnostic writes, bounded decoding, local effective policy and
repair receipt storage. Keep new repair execution inaccessible until W4 is
complete. Preserve index blob version and argv fingerprints.

**Exit evidence:** upgrade tests with legacy failures and valid artifacts;
stale-fence rejection writes neither diagnostic nor state; both backends
round-trip bounded diagnostics and receipt transactions.

### W2 — selected-video completion and process result

Implement expectation resolution, bounded held-source probe and reader/
process result combination. Cover standalone and attested/cluster builders,
including non-Unix code paths. Retain last progress and reap every child.

**Exit evidence:** old predicate fails/new predicate succeeds on the longer-
audio fixture; a shortened video remains refused; metadata/process matrix
passes; existing DV/index determinism regressions pass unchanged.

### W3 — durable bounded retry

Apply §5 to failure, claim, enqueue, rebind, sweep and priority-upgrade paths.
Mirror local disposition and update request settlement/read models to show
the job's waiting/terminal state. Audit all attempt/deadline resets.

**Exit evidence:** R1 regression passes on both Stores; crossing six hours
does not expire scheduled work; maximum attempt or seven-day limit is final
under repeated discovery; preemption and foreground work cannot reset it.

### W4 — identity-correct retry and one-time repair

Fix generic retry identity propagation and deduplication first. Implement
legacy attribution, standalone-job candidates, preview cursor and exact
candidate apply. Add receipts and transaction-level headroom/source/head
checks. Keep original request/job history and skip positive artifacts.

**Exit evidence:** R2 regression passes on both Stores; concurrent/repeated
apply creates one successor per intended identity; original failed rows
remain; no pipeline sibling is rebuilt or suppressed accidentally.

### W5 — operator surfaces and operational documentation

Wire diagnostics, retry state and repair preview/apply through the existing
analysis page. Update browse summaries and the maintained
[API reference](../API.md) and [operations reference](../OPERATIONS.md).
Add response-contract and UI tests, including absent legacy diagnostics.

**Exit evidence:** timeout, unverified, shortfall and legacy failures render
distinct truthful messages from any node; preview cannot mutate; stale
candidate application reports its skip without substituting another file.

### W6 — integration, review and fleet acceptance

Rebase/merge the intended current base according to repository workflow,
rerun focused compilation/tests against that exact tree and obtain the
required adversarial implementation review. Address findings before the
main-bound candidate is marked ready. Qualify playback/tails and prepare a
concrete preview report before any authorized fleet recovery.

**Exit evidence:** exact source SHA, migration versions, compile/test results,
review dispositions, compatibility inventory and rollout/rollback procedure.
Implementation complete and fleet acceptance complete are separate ledger
states; neither may be inferred from a merge or green aggregate queue badge.

## 11. Compiler and test commands

Follow [the compiler loop](../ci/AGENT-COMPILE-LOOP.md) and
[development pipeline](../DEVELOPMENT_PIPELINE.md). The workflow correction
at the top of the latter says draft main-bound PR, one adversarial review,
address findings, then mark ready to start the fast lane; merging does not deploy.
Where the supplied AGENTS rules additionally require effort/promotion
evidence, satisfy those applicable gates too. Do not use CI as the compiler.

Run these on the exact candidate in the pinned compiler environment. These
are implementation commands, **not checks claimed run while writing this
document**. `content_analysis_` tests are to be added by the packages above;
zero matched tests is a failure of the validation command, not a pass.

```bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo fmt --all -- --check
rustup run 1.97.1 cargo check -p plurxd --all-targets --locked
rustup run 1.97.1 cargo clippy -p plurxd --all-targets --locked -- -D warnings
rustup run 1.97.1 cargo test -p plurx-core --locked content_analysis_
rustup run 1.97.1 cargo test -p plurx-core --locked --features hiqlite-contract-tests content_analysis_
rustup run 1.97.1 cargo test -p plurxd --bin plurxd --locked content_analysis_
rustup run 1.97.1 cargo test -p plurxd --bin plurxd --locked fragindex::tests
rustup run 1.97.1 cargo test -p plurxd --bin plurxd --locked a_seek_into_the_audio_tail
node --test tests/web/content-analysis-failures.test.js
python3 tests/operations/test_docs_index.py
```

The web test path is a new required file. Put backend-neutral Store tests
where the real Hiqlite contract harness selects them, and record the actual
SQLite/Hiqlite test counts. Include affected core-feature Clippy and the
repository's selected Windows/non-Unix compile checks because the held-source
builder differs there. Run required static web/catalog checks and add the
appropriate regression mapping for the corrections.

For a remote compiler, archive committed source with `git archive`; transfer
neither `.git` nor credentials. Keep `target/` warm. After applying to a moved
base, archive and rerun the exact branch rather than citing old evidence.

## 12. Rollout, observation and rollback

Reserve migration numbers, document the upgrade sequence and use the
repository's fleet/schema procedure. A schema-compatible binary is not
policy-compatible with old workers that still expire six-hour retries and
drop identities. Do not enable new repair/retry behavior on a mixed fleet.
Schedule the required coordinated upgrade separately from code construction;
this handoff does not instruct the implementer to restart the live fleet.

After an authorized deployment, verify the same approved build and migration
versions on every worker and observer. Preserve the operator's existing
analysis enablement state. Preview only the four duration examples first,
showing source/engine/video identities and any already-ready skips. Apply
those exact eligible candidates when recovery is authorized.

Require current artifacts to publish for those cases, and verify selected
audio, end-of-title and seek behavior. Then preview a 50-file batch and
record its exact identity count, known/unknown causes and skips. Observe
each identity to ready, precisely terminal, or explicitly pending before
expanding. Pending work does not count as success.

The acceptance record must include pre/post build and source identities,
charged attempts and deadlines, coverage/provenance, artifact/head result,
actual playback checks and why any skip occurred. Report a subsequent
24-hour cohort of newly terminal attempts separately from retained history;
do not claim the original 45% was reproduced if it was not measured.

Stop further batches for false-success short reads, changed-source
publication, identity mixing, lost diagnostics/fences, retry-budget reset,
new unexplained metadata refusals in the successful cohort, or playback/tail
regression. Preserve evidence and valid artifacts. Use existing controls to
pause new work; cancellation must revoke active claims safely.

Rollback is to a binary compatible with the applied schema and retry-cycle
semantics. Do not run an old worker that would expire/reset new-policy jobs,
drop diagnostic fields or misinterpret repair successors. If no such binary
exists, keep analysis paused and roll forward with a correction. Do not
delete receipts, clear the queue, restore an old database over current
cluster state or invalidate the whole library as a rollback shortcut.

## 13. Completion checklist

- [ ] W0 reproductions and successful-file timing inventory recorded.
- [ ] Video-only completion is used by every production index entry point.
- [ ] Real short reads, nonzero exits and stale sources cannot publish.
- [ ] R1 closed by both-backend clock/upsert/deadline regressions.
- [ ] R2 closed by exact-identity candidate and successor regressions.
- [ ] Diagnostics persist atomically and read consistently across nodes.
- [ ] Repair preview/apply is bounded, idempotent and race-safe.
- [ ] Positive artifacts and recipe fingerprints remain unchanged.
- [ ] Actual pinned compilation, focused tests and review recorded.
- [ ] Authorized fleet sample and bounded cohort results recorded separately.
- [ ] Maintained references, docs index and final limitations updated.
