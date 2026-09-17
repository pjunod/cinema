# Content analysis failures — complete video indexes rejected as incomplete

**Status:** open; root cause established for sampled duration failures; fix
proposed, not implemented or deployed · **Written:** 2026-09-17 UTC
(2026-09-16 Eastern) · **Review audience:** Fable.

Companion to [queue repair verification](QUEUE-REPAIR-VERIFICATION-PROMPT.md)
(the earlier queue recovery) and
[Wicked startup](STUTTER-4K.md) (a separate playback failure
while an index was unavailable). This document explains the retained
“Index output was incomplete” failures and proposes their correction.
It is a review proposal, not authorization to deploy or reopen the fleet.

**Executes:** diagnosis of Paul's report that roughly 45% of content-analysis
index attempts appear failed. Implementation must preserve source attestation,
lease fencing, partial-output rejection and existing valid artifacts. The
review decisions in §8 must be settled before this becomes a build contract.

## 1. Finding — the completion check measures the wrong stream

The index pipe copies video only, but its completion check compares the
summed video sample durations with `MediaFile.duration_ms`, the catalogued
container duration. A longer audio track or another container-duration
contributor can therefore make a complete video pass look truncated.
The check allows only two seconds of difference.

This is directly demonstrated by file 9, **Avatar: Fire and Ash**. Its index
covers exactly the duration reported for the primary video. The checker
instead expects the duration of the longer French audio track and refuses
the result. Retrying unchanged code reproduces the same refusal.

Two additional defects compound the problem:

1. **Timeouts share the same failure category.** Exceeding a processing
   budget becomes `IndexOutcome::Truncated`, despite establishing nothing
   about source completeness. The cluster marks every such outcome
   non-retryable, while the local outcome store has a retry/backoff policy.
2. **The UI hides the distinction.** `truncated` displays “Index output was
   incomplete” and advises checking/replacing damaged media. That advice
   is not supported by either a duration mismatch or a timeout alone.

The duration bug is the dominant cause in the retained node-local records
examined here. These records do **not** establish that 45% of fresh attempts
are failing, or that every historical failed job has this cause.

## 2. Evidence — live records and the deployed implementation

### 2.1 Build and collection scope

All four nodes reported `v0.3.0-2633-gc9e4edf4` during the investigation:
`nynuc`, `m6`, `nuc4` and `nuc3`. The full deployed commit is
`c9e4edf451e12247a7aa4188903e5ba36888e7e9`.

Evidence came from read-only SSH queries to the replicated database at
`/srv/plurx/hiqlite/state_machine/db/plurx.db`, each node's
`/srv/plurx/hiqlite/telemetry.db`, `/metrics`, and Docker logs. No queue rows,
media, configuration or runtime processes were changed.

The local documentation checkout is older and contains unrelated edits.
The relevant deployed implementation was checked with `git show c9e4edf4`.
Source links below are navigation aids; re-verify symbols on the intended
implementation base rather than relying on this checkout's line numbers.

### 2.2 Retained refusal records

| Node | Duration mismatch | Budget timeout | Mismatches with usable video-duration metadata | Within 2 seconds of that video duration |
|---|---:|---:|---:|---:|
| nynuc | 100 | 31 | 97 | 97 |
| m6 | 101 | 39 | 98 | 98 |
| nuc4 | 96 | 27 | 93 | 93 |
| Total | 297 | 97 | 288 | 288 |

The queried outcome table on nuc3 was empty. Of 394 retained refusal
records on the other nodes, 75.4% were duration mismatches. All 288
duration-mismatch records with usable primary-video metadata agreed with
the indexed coverage to within two seconds. Nine lacked usable metadata.

**How to read this:** these are node-local outcome rows, keyed by file and
pipeline fingerprint, not unique files, unique cluster jobs or a complete
attempt history. The same file can occur on several nodes or pipelines.
Successful later builds can remove local refusal records. These counts are
a retained-failure cohort, not an unbiased failure-rate measurement.

The comparison used the stored probe's non-attached primary video, its
`duration` or `DURATION`/`DURATION-eng` tag, and the refusal's covered/expected
tick ratio scaled by catalogued duration. Matching metadata strongly
supports the completion-check diagnosis; it is not an independent full
decode or a fresh packet-by-packet integrity check of all 288 records.

### 2.3 Concrete duration mismatches

The examples below use the 16,000-tick-per-second output timebase reflected
by their expected ticks and catalogued durations. All durations are seconds.

| File | Title | Indexed video | Probe video | Container expectation | Excess expectation |
|---|---|---:|---:|---:|---:|
| 9 | Avatar: Fire and Ash | 11824.916 | 11824.916 | 11844.896 | 19.980 |
| 3451 | Dexter S02E11 | 3089.006 | 3089.008 | 3117.664 | 28.658 |
| 3498 | Dexter S06E10 | 2957.287 | 2957.288 | 2966.298 | 9.011 |
| 3634 | Family Guy S19E04 | 1297.045 | 1297.046 | 1301.772 | 4.727 |

Avatar's retained reason is `covered 189198656 of 189518336 ticks`, with
4,520 fragments; its latest retained refusal was 2026-09-16 15:42:02 UTC.
The French audio duration is `11844.896`, exactly the container expectation.
The Dexter examples also carry audio longer than video. Family Guy's
container exceeds both reported primary video and audio durations; the
specific contributor was not established.

### 2.4 Timeout evidence and the limits of the reported percentage

The deployed budget is:

```text
film_seconds = ceil(positive catalogued duration_ms / 1000), or 0
budget_seconds = clamp(ceil(film_seconds / 8) + 30, 90, 1800)
```

This assumes approximately 8× playback speed, adds 30 seconds and caps the
pass at 30 minutes. File 120, **Wicked**, retained an `exceeded the 1232s
index budget` refusal from 2026-09-15 23:30:35 UTC. At 2026-09-17
01:06:46.358316 UTC, nynuc logged a successful build for file 120:
12,247 fragments, timescale 16,000 and elapsed time 812,146 ms.

That proves a later index build for this file succeeded. The collected log
and refusal do not establish that both attempts used the same video pipeline
or unchanged source identity. No storage trace proves why the earlier pass
was slow; NFS contention, host load and pipeline cost remain hypotheses.

The replicated job snapshot contained:

| State / last error | Rows |
|---|---:|
| ready | 5543 |
| failed / attempt_limit | 2437 |
| failed / queue_expired | 710 |
| failed / truncated | 455 |
| failed / unsupported | 97 |
| failed / source_superseded | 3 |
| failed / pipeline_superseded | 2 |

This is historical job state, not the denominator behind Paul's UI estimate.
The sampled 24-hour metrics reported 8 ready against 10 claimed, with the
queue classified healthy. Claims and completions can cross time boundaries;
do not turn that ratio into an exact per-attempt success rate either.
The historical failures remain worth repairing even while fresh work builds.

## 3. Causal chain — construction, classification and persistence

| Source | Existing behavior relevant to the incident |
|---|---|
| [Core transcode arguments](../../crates/plurx-core/src/transcode/mod.rs) | `copy_index_pipe_args_with_input` maps `0:v:0?`, drops audio/subtitles and chapters, and emits fragmented MP4 |
| [Index builder](../../crates/plurxd/src/fragindex.rs) | `build_with_args` passes `file.duration_ms` as `expected_ms`; `index_stream_with_progress` rejects `covered + timescale * 2 < expected` |
| [Fragment timing](../../crates/plurx-core/src/fmp4.rs) | `video_duration` sums video sample durations; it does not measure the longest container track |
| [Worker orchestration](../../crates/plurxd/src/state.rs) | `index_file_budget` sets the deadline; the cluster `Truncated` branch records the local reason then calls `fail_fragment_index_job` with code `truncated` and `retryable = false` |
| [Local outcome persistence](../../crates/plurx-core/src/store/fragindex.rs) | Records reason, attempts and rows; local truncated refusals back off from 30 minutes toward a 24-hour ceiling |
| [Analysis HTTP handlers](../../crates/plurxd/src/http/analysis.rs) | Reopen creates successors subject to current-source, active-successor and queue-headroom checks |
| [Web analysis page](../../crates/plurxd/src/web/index.html) | `analysisErrorInfo` maps `truncated` to the generic incomplete-output message and source-replacement advice |

```text
Complete video-only pass
          |
          v
Compare against longer container duration
          |
          v
Truncated(reason = covered X of Y ticks)
          |
          +----> node-local refusal with backoff
          |
          +----> cluster terminal failure, retryable = false
                          |
                          v
              UI suggests damaged/truncated source
```

Timeout, pipe-read/spawn failure and process-exit problems also enter the
same broad `Truncated` category. FFmpeg stderr is drained but logged at debug
level; timeout outcomes report zero rows even if the pass made progress.
This loses useful evidence and makes operational failures look like media
integrity findings.

## 4. Proposed correction — validate the selected video without weakening integrity

### 4.1 Use a video-specific completion expectation

Introduce one explicit completion-expectation value carried into the index
pass: selected stream identity, expected duration when known, provenance
and confidence. Do not overwrite `MediaFile.duration_ms`; other consumers
may correctly need the container runtime.

The expectation must describe the exact stream selected by the production
argv. At the deployed commit this is `0:v:0?`. Do not independently choose
“first non-attached video” for validation while FFmpeg maps a different
stream. Cover-art-first and multiple-video files require an explicit match
or a typed refusal. Changing stream selection itself is a separate behavior
change unless the same selector is adopted by index and playback together.

Proposed metadata order, subject to Fable's review:

1. Positive, finite selected-stream `duration_ts × time_base`, using checked
   rational arithmetic and a documented rounding rule.
2. Positive, finite selected-stream `duration` when equivalent timing
   semantics are established.
3. Selected-stream Matroska `DURATION`, then `DURATION-eng`, parsed strictly
   as hours/minutes/seconds; retain provenance because tags can be stale.
4. Otherwise an explicit unknown expectation, never an implicit substitution
   of the longest container duration.

Bind metadata to the currently attested source identity. A cached probe from
different size/mtime must not validate a new file. If metadata conflicts,
perform a bounded probe against the held source using existing source-access
helpers. A timeout there is an operational failure, not proof of truncation.

**Unknown/conflicting metadata policy is a review decision.** The conservative
proposal is to refuse publication with `index_completion_unverified` after a
bounded verification attempt, without calling the file corrupt. Approving
publication on clean FFmpeg EOF alone requires an explicit alternative
completeness argument and regression coverage. Do not silently pass `None`
to disable a previously effective completeness guard.

Keep the two-second allowance for the initial correction. Establish that
stream duration and summed output durations use comparable semantics for
nonzero starts, edit lists, timestamp gaps and variable frame rate. If they
do not, reject ambiguous evidence or explicitly normalize the timeline;
do not widen the tolerance to hide the mismatch.

Publication still requires complete fragment parsing, acceptable codec and
parameter-set invariants, successful process exit, current source identity
and the held lease. A valid prefix, a trailer, or a matching duration alone
is not sufficient proof of completion.

### 4.2 Give failures typed causes and bounded retry policy

Split the internal outcome into typed causes before persisting it. New
durable codes must be accepted by both Store backends, API consumers and
the UI; names below are proposed, not existing interfaces.

| Proposed cause | Proposed disposition | Operator meaning |
|---|---|---|
| `index_budget_exceeded` | Retry with bounded backoff and existing charged-attempt ceiling | Processing ran out of time; no source-damage conclusion |
| `index_source_io` | Bounded retry for classified transient errors | Source read failed; inspect mount if persistent |
| `index_video_shortfall` | Terminal for unchanged source after authoritative expectation is established | Output did not cover the selected video's expected duration |
| `index_completion_unverified` | Requires metadata verification; no repeated whole-file loop | Available evidence cannot establish completeness |
| `index_output_malformed` | Terminal unless an identified transient process failure explains the partial output | Produced data cannot form a valid index |
| `index_process_failed` | Classify by bounded process diagnostics; no blanket retry for unsupported input or missing executable | FFmpeg did not finish successfully |

Keep foreground preemption and lease loss in their existing cancellation/
yield paths; they must not become charged content failures. Derive local and
cluster dispositions from the same policy so they cannot disagree about a
timeout again. Preserve lease fences and finite attempt limits.

For the first patch, retain the current per-attempt budget formula and
30-minute ceiling. A timeout should retry after the existing local schedule
of 30 minutes, doubling toward 24 hours, subject to the queue's configured
attempt ceiling. Measure progress and throughput before changing budgets;
retries alone will not fix a pipeline that is consistently slower than 8×.

Persist bounded structured diagnostics: cause, selected stream, expectation
provenance, expected/covered milliseconds, last fragment count, bytes read,
elapsed/budget milliseconds, process exit status and a sanitized stderr
tail. Capture progress outside the timed future so timeout retains its last
values. Proposed limits are 16 stderr lines and 4 KiB total, with credentials
and source paths redacted from the API. Do not block the stderr drain.

Diagnostics must remain visible when another node serves the admin request.
Prefer a versioned bounded payload in existing durable attempt storage if
its contract permits; otherwise propose an additive schema migration on both
backends. Do not assume a node-local reason can be joined from every node.

### 4.3 Show the reason instead of guessing source damage

Display “Indexing timed out” with elapsed/budget and retry status for timeouts.
Display “Video duration did not match” with measured and expected duration
for shortfalls. An old `truncated` record without richer evidence should say
“Indexing did not complete; the original record does not identify the cause.”
Remove automatic advice to replace media from this generic category.

Keep historical failures visible, but distinguish them from current work and
new attempts. Verification should report unique current source/pipeline
identities and terminal attempts from a stated observation window, with
pending attempts listed separately. Do not call failed history divided by
all history the current failure rate.

## 5. Recovery — repair stranded identities without rebuilding the library

Changing validation alone does not reopen terminal cluster jobs. Recovery
is a required part of the change, not a later cleanup task.

Preserve valid artifacts and the existing pipeline fingerprint when emitted
bytes are unchanged. Do not bump an argv fingerprint merely to invalidate
negative outcomes: that would needlessly invalidate successful indexes too.

Build a bounded, dry-run-first candidate operation selecting current
source/pipeline identities whose terminal legacy `truncated` failure has no
ready or active successor. Where retained details exist, distinguish known
duration mismatches and timeouts; otherwise mark the original cause unknown.
Create at most one recovery successor per identity and repair revision.
Keep original history, and never directly edit production SQLite rows.

The deployed `POST /api/v1/analysis/reopen` accepts `dry_run`, `limit` and
`component`; it defaults to preview, defaults to 50 files and caps at 500.
It has **no error-code filter** and excludes standalone cluster jobs without
an analysis request. Reusing it unmodified cannot prove targeted recovery.
Extend the supported Store/API path or provide a reviewed reconciliation
operation covering those jobs, with the same identity and lease checks.
Specify how retry bypasses the matching local refusal/backoff without
clearing unrelated identities, and test it on both Store implementations.

Start acceptance with files 9, 3451, 3498 and 3634 after confirming their
source identities still match. Then preview a batch capped at 50 distinct
files, showing identity count separately. Preserve the existing queue
headroom and successor-dedup rules. Apply the batch only as part of a
separately authorized rollout, inspect its outcomes, and then expand.
Unknown legacy cases may legitimately fail again with a precise new cause;
recovery must not relabel those as successes.

## 6. Work packages and acceptance evidence

| Package | Deliverable | Required acceptance |
|---|---|---|
| A — completion expectation | Source-bound, stream-matched metadata resolution and completion validation | Avatar-shaped fixture fails under old code and passes with all video intact; deliberately shortened video still fails |
| B — failure policy | Typed causes, shared disposition, bounded diagnostics and cross-node visibility | Timeout preserves progress, schedules a bounded charged retry, eventually respects attempt limit, and cannot publish partial output |
| C — operator surface | Cause-specific messages and accurate retry state | UI tests cover timeout, shortfall, missing metadata and legacy `truncated`; no unsupported source-replacement advice |
| D — recovery | Targeted preview and idempotent successor creation | Both backends cover existing success, active successor, changed source, multiple DV identities, standalone job, repeated recovery, headroom and stale owner |
| E — fleet qualification | Sample reindex plus bounded recovery cohort | Samples publish valid indexes; playback starts, seeks near the end and completes without a newly introduced missing-video or A/V-tail failure |

The focused regression matrix must include:

- MP4 with longer secondary audio; MKV with longer audio; subtitle/container
  tail; exact two-second boundary; malformed and conflicting duration tags.
- Multiple video streams and attached pictures before the actual video;
  rational timebases, nonzero start, edit lists, timestamp gaps and VFR.
- Missing duration without silent guard removal; stale metadata rejected
  against the held source identity.
- FFmpeg nonzero exit after a valid prefix; partial final fragment; timeout
  after progress; child-process cleanup; lease loss and playback preemption.
- Ordinary H.264/HEVC and applicable stripped, preserved and converted Dolby
  Vision identities. Correct video indexing must not alter sample bytes,
  fragment boundaries, parameter-set checks or fingerprint determinism.

Review end-of-title behavior explicitly. Accepting a complete video index
does not by itself define what playback should do with 20 seconds of trailing
audio. Establish the existing player/segment-plan policy and prove the
correction preserves it; raise any required policy change separately.

Follow [the development pipeline](../DEVELOPMENT_PIPELINE.md) and establish
[the pinned compiler loop](../ci/AGENT-COMPILE-LOOP.md) before editing Rust.
Run focused behavior regressions, affected compile checks, Clippy and
formatting on the exact candidate base before pushing. Record actual test
names and commands in the implementation PR; none are claimed run here.

Fleet qualification must record build, source identity, video identity,
cause, attempt times, coverage and publication result. Observe the bounded
recovery cohort until each identity is ready, precisely failed, or explicitly
still pending. Sample a subsequent 24-hour window for recurring timeouts and
new shortfalls. A green aggregate health label alone is not acceptance.

## 7. Scope and rollout limits

This repair does not replace media, rescan every library, change video
selection independently of playback, increase worker concurrency, rewrite
fragment cutting, or fix all historical `attempt_limit`/`queue_expired`
failures. Those actions would obscure whether the identified bug was fixed.

Deploy compatible code to all workers before beginning recovery. If new
replicated fields are necessary, follow the repository's actual migration
and fleet-upgrade rules; do not assume a mixed-version rollout is safe.

Stop additional recovery batches if malformed artifacts publish, true short
reads pass validation, retry volume fails to remain bounded, or playback
regresses. Preserve evidence and valid artifacts. Roll back only to a build
compatible with any applied schema; do not erase diagnostic history or
blindly downgrade a migrated database.

## 8. Fable review — decisions needed before implementation

1. **Causal strength:** does the source comparison plus exact Avatar match
   establish the proposed root cause, with the cohort limitations stated
   accurately? Is any additional targeted media reproduction needed first?
2. **Timing authority:** approve or revise the metadata precedence,
   source-identity binding and normalization rules. Are Matroska tags
   sufficient evidence, or should they require another check?
3. **Unknown timing:** approve conservative `index_completion_unverified`,
   or specify a bounded alternative that proves completeness without the
   container-duration comparison or an unconditional second whole-file pass.
4. **Retry contract:** approve the typed cause/disposition split, retained
   budget formula, bounded backoff and attempt accounting. Identify errors
   that would otherwise create futile retry loops.
5. **Persistence:** choose the existing storage extension or additive schema
   contract that exposes bounded diagnostics from every node, including
   jobs without operator requests and old records without details.
6. **Recovery:** approve a targeted successor operation, its repair revision,
   handling of node-local refusals and standalone jobs, and how it avoids
   invalidating valid artifacts or reopening the same identity repeatedly.
7. **Playback acceptance:** confirm that video-duration indexing preserves
   the intended end-of-title behavior for longer audio/subtitle tracks.

Return findings with severity and a concrete correction to the relevant
section. Implementation readiness requires these contracts to be settled;
the diagnosis alone is not a completed repair.
