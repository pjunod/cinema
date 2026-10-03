# Heated Rivalry S1E5 — why index cleanup stopped playback across the cluster

**Status:** initiating database stall identified and reproduced; fix proposed,
not implemented or deployed · **Incident and investigation:** 2026-10-01 EDT ·
**Deployed source:** `3a512c8909ee8c2814ab2004ed724c7b39bdd8ac`
(`v0.3.0-5373-g3a512c890`).

Companion to [the earlier stall repair](PLAYBACK-STALL-RECOVERY-IMPLEMENTATION.md)
and [PLAYBACK.md](../PLAYBACK.md). The earlier repair added the expiry evidence
that made this attribution possible. This document answers why the leader
could not refresh its proof during the later S1E5 incident, why the browser
showed different errors, and what a bounded corrective change should do.
Use the [development pipeline](../DEVELOPMENT_PIPELINE.md) for implementation.

## 1. Finding — a cleanup query blocks replicated application for over a second

The primary cause is the final `DELETE FROM cluster_fragment_index_jobs` in
`prune_cluster_fragment_indexes`. Its correlated lookup into
`analysis_requests` lacks an index usable by the predicate as written. For
each qualifying old failed job, SQLite searches the requests belonging to
the target node and tests their cache keys. On the incident dataset, the
statement executes approximately **52.84 million SQLite VM instructions**,
even though it deletes **zero rows**.

This is a replicated write. While its SQLite writer executes, the state
machine cannot finish applying that Raft entry. The leader's
`ensure_linearizable()` includes waiting for application through the read
index. Proof requests therefore wait behind the cleanup. The 750 ms sample
deadline and one-second serving lease expire, causing all three voters to
fence mutable playback. No election or dead server is required.

The attribution has three independent supports:

1. **Production correlation by Raft index:** for eight retained incidents,
   all three voters stopped reporting application at the same index. The
   very next WAL entry was this cleanup statement in every case.
2. **Reproduction with production data:** on an isolated in-memory snapshot
   of the six relevant tables, indexes and planner statistics, the exact
   statement takes **1,222–1,330 ms** with the daemon's pinned SQLite version,
   3.53.2. It affects zero rows in every trial.
3. **A controlled change removes the cost:** adding only an unconditional
   index on `(result_cache_key, target_node_id, force_rebuild)` reduces the
   same statement to **1.68–1.96 ms**, still affecting zero rows. Its median
   time falls from 1,257.82 ms to 1.77 ms, about **712× faster**.

**Recommended first fix:** add that lookup index through both supported
schema migration paths, and add a regression that exercises the exact
production candidate read and final DELETE with many retained failed jobs. Preserve the serving
fence and its safety budget. Making the proof wait longer conceals the slow
database operation and changes the partition-safety contract.

The benchmark is SQL evidence, not a deployed fix or end-to-end Raft
acceptance. Section 4 states its limits. Older quorum incidents are not all
attributed by this investigation.

## 2. Incident — the different errors followed the same stream teardown

The viewer used Safari at `http://m6:32400`, playing file **6736**, Heated
Rivalry S1E5, “I'll Believe in Anything.” The file is 6,504,792,644 bytes,
3,339.36 seconds, HEVC Main 3840×1600 SDR video and six-channel E-AC-3 audio.
FFprobe read it successfully; FFmpeg decoded its opening 15 seconds without
error. This does not validate the entire file or prove Safari compatibility.

The first log capture contained **19 serving-authority losses on each of
m6, nuc4 and nynuc** in approximately 45 minutes. The first captured loss
was around 19:41 EDT, well before this episode's first recorded decision at
20:21:37. The media file did not initiate the cluster-wide problem.

All times below are **2026-10-01 EDT**. Server logs use UTC on October 2;
subtract four hours. Cross-node attribution uses equal Raft indices rather
than assuming clocks are perfectly synchronized.

| Time | Evidence | Consequence |
|---|---|---|
| 20:21:40.069 | m6 logs serving authority expired | The initial HLS creation receives HTTP 503 at 20:21:41.035 |
| 20:22:08.757 | VOD refuses with `vod_index_pending`; temporary copy-HLS starts | The player depends on an active mutable producer |
| 20:22:41.073 | m6 fences; the episode's session logs self-fencing at 20:22:41.342 | The server terminates that producer |
| 20:22:44.105 | Safari reports native media error 3 and requests a transcode fallback | A decoder-labelled error follows the server teardown by about 2.8 seconds |
| 20:22:46.644 | The fallback receives `409 candidate_recipe_changed_or_decoder_unavailable` | The replacement does not open |
| 20:23:20.549 | A fresh attempt again selects temporary copy-HLS because the index is pending | Refresh has created another stream vulnerable to the same periodic operation |
| 20:23:40.558 | m6 fences again; the new session is retired | Another attempt loses its producer |
| 20:24:03.784 | Web diagnosis says “The stream request looks blocked,” with ad-blocker advice | The UI does not preserve an accurate explanation of the server interruption |
| 20:24:25.001 | Another attempt again falls back from VOD | The sequence repeats |
| 20:24:40.073 | Serving authority expires again | The replacement is terminated |
| 20:25:08.418 | The ad-blocker diagnosis appears again | A second misleading diagnosis follows a known server failure |
| 20:25:30.181 | Retry reports `503: the local media worker has no serving authority` | The actual cluster failure surfaces again |

The leader was nuc4, Raft node **6**, in term **18070**. Its container had no
restart or OOM event in the inspected status. CPU quota and cpuset were
unset, and the sampled host pressure did not show saturation. Those are
point observations, not a claim that resources can never contribute.

### 2.1 The eight surviving WAL records identify the blocking statement

For each row, the passive metrics captured at authority expiry reported the
listed last-applied index on all three voters. The retained leader WAL was
read without modifying it. At `last_applied + 1`, its SQL payload begins
`DELETE FROM cluster_fragment_index_jobs` and contains the terminal-job
retention predicate reproduced in section 4.

| m6 expiry time, EDT | Last applied | Next WAL entry: slow prune |
|---|---:|---:|
| 20:20:40.070 | 26358371 | 26358372 |
| 20:21:40.069 | 26358665 | 26358666 |
| 20:22:41.073 | 26358880 | 26358881 |
| 20:22:51.558 | 26358916 | 26358917 |
| 20:23:40.558 | 26359111 | 26359112 |
| 20:23:50.558 | 26359141 | 26359142 |
| 20:24:40.073 | 26359294 | 26359295 |
| 20:25:29.067 | 26359416 | 26359417 |

The older eleven losses remain in the daemon logs, but their next entries
were no longer found in the retained WAL files during this read. Do not
claim an eight-entry attribution proves every earlier incident. In
particular, the earlier S1E1 incident needs its own retained evidence before
assigning it this cause.

The earlier losses also supply circumstantial cadence evidence:

| Approximate phase | Captured UTC times, October 1 → October 2 | Cadence |
|---|---|---|
| :20 | 23:41:20 → 23:57:20 → 00:13:20 | 16 minutes |
| :40 | 23:44:40 → 00:00:40 → 00:16:40 | 16 minutes, then approximately every minute |
| :50 | 23:50:51 → 00:06:49 → 00:22:51 | Approximately 16 minutes, then another loss at 00:23:50 |

The three phases fit independent node discovery schedules: a 15-minute
interval plus the next 60-second scheduler tick after completion. Taken
with the code and later WAL correlations, they suggest that normal discovery
on every node was already fencing the cluster throughout this 45-minute
capture, averaging roughly one outage every five minutes before the faster
repeats. The busy-worker loop multiplied that rate; it did not start the
problem. This is a cadence inference, not proof assigning each earlier loss
to a node or a retained WAL statement.

The 00:18:28 and 00:25:29 events fit none of these phases. The latter has a
retained cleanup WAL correlation, but its off-cadence trigger remains
unexplained. The timestamp sequence alone does not establish that every
normal pass on every node executed the offending statement.

The deployed warning says `linearizable_check` because it does not separate
quorum-heartbeat time from application time. This document's finer
attribution comes from the Raft-index boundary, the writer code and the
isolated SQL reproduction; it is not a hidden timing field in that warning.

## 3. Mechanism — unbounded lookup work inside a bounded delete

### 3.1 The missing access path

The relevant source is
[the replicated prune implementation](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs),
with a corresponding implementation in
[the SQLite Store](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs).
Both check whether a failed job has a forced analysis request:

```sql
EXISTS (
  SELECT 1 FROM analysis_requests request
  WHERE request.result_cache_key = terminal_job.cache_key
    AND request.target_node_id = terminal_job.target_node_id
    AND request.force_rebuild = 1
)
```

Production already has this **partial** history index:

```sql
CREATE INDEX analysis_requests_result_history
ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
WHERE result_cache_key IS NOT NULL AND result_cache_key <> '';
```

The cleanup predicate does not explicitly establish `result_cache_key <> ''`.
SQLite does not infer that partial-index predicate from the join to a job
cache key. The reproduction engine’s plan against the read-only production
file (not a plan exported by the daemon) instead contains:

```text
SEARCH request USING INDEX analysis_requests_due (target_node_id=?)
SEARCH active_request USING INDEX analysis_requests_status (state=?)
```

Those indices find a node's requests or a state's requests, not the matching
cache key. The incident snapshot contains 18,092 fragment-index jobs and
27,433 analysis requests. A later count found 1,590 failed jobs older than
the 30-day cutoff; active-node request populations were around 7,000 each.
Repeated filtering of those populations explains the approximately
52.84 million VM instructions.

`LIMIT 128` bounds the number of deleted rows. It does **not** bound the
work needed to determine that no rows qualify. Retained failed jobs remain
eligible for this expensive check on every later cleanup pass.

The preceding candidate read uses `query_consistent_map` with the identical
forced-request `EXISTS` predicate before the write transaction. Its LIMIT
also does not bound the work of ruling out candidates. It runs on every
pass and likely consumes leader reader time of the same order as the
measured DELETE (about a second), although this investigation did not time
that read separately. It does not block state-machine apply as the replicated
writer does. The same index repairs its access path; plan and work-budget
regressions must cover both actual statements.

The plan also contains a scan of `cluster_fragment_index_heads`. An initial
isolated test added an index on `generation_cache_key` alone: execution
remained about 1.49 seconds with 52.75 million VM instructions on SQLite
3.46.1. That access path is worth auditing for other datasets, but it is not
the measured dominant cost here. Adding only the analysis-request lookup
index is sufficient for the incident reproduction on SQLite 3.53.2.

### 3.2 SQL application blocks the proof, which revokes existing sessions

[Hiqlite transactions](../../vendor/hiqlite/src/client/transaction.rs) submit
the cleanup through `client_write(QueryWrite::Transaction(...))`.
[The state machine](../../vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs)
awaits the SQLite writer's transaction response before completing the apply.

[The leader watermark](../../vendor/hiqlite/src/client/mgmt.rs),
`db_quorum_watermark_local`, calls OpenRaft 0.9.25's
`ensure_linearizable()`. That API obtains a read log ID, then waits for the
state machine to apply through it. The application wait is part of a valid
linearizable read, not proof that a network partition occurred.

[The proof sampler](../../crates/plurx-core/src/cluster/migration.rs) uses:

| Budget | Value | Meaning |
|---|---:|---|
| Refresh interval | 500 ms | Start the next sample on this cadence |
| Sample deadline | 750 ms | Bound a single proof request |
| Serving proof lease | 1,000 ms | Measured from the successful request's start |
| Leader linearizable-call deadline | 1,000 ms | Bound the leader's combined check/apply wait |
| Serving-fence poll | 25 ms | Observe expiry without another Store call |

In an aligned cycle, the preceding proof has only about 500 ms left when
the next sample starts. The producer can be fenced before that new sample's
750 ms timeout is logged. The captured attempts had approximately zero or
one millisecond of tick lateness, so a late sampler start is not the
explanation for these events.

```text
fragment-index discovery
        │
        ▼
replicated retention DELETE
        │ repeated node-wide analysis-history lookups, ~1.26 seconds
        ▼
state-machine application stops at entry N
        │ next entry N+1 is the cleanup
        ▼
leader linearizable reads wait for application
        │
        ▼
all voters' serving proofs expire
        │ loss_generation increases
        ▼
rolling producer is permanently retired
        │ recovery of cluster authority cannot revive that old generation
        ▼
Safari encounters a dead stream and begins a failing recovery sequence
```

[Serving authority](../../crates/plurxd/src/serving_fence.rs) deliberately
retains a monotonically increasing loss generation. A quick recovery does
not make an actor admitted under the previous generation valid again. That
is the amplification mechanism, and it is a safety rule to preserve during
this repair.

### 3.3 Busy playback can turn a 15-minute job into minute-by-minute cleanup

[The scheduler](../../crates/plurxd/src/schedule.rs) sees fragment indexing
as due after `playback.vod_index_mins`, configured to 15. Its scheduler tick
runs every 60 seconds. Discovery acquires one of two cluster-wide permits,
`media:fragment-index:{0,1}`, so at most two nodes prune concurrently. The
permits bound concurrency, not the frequency or cost of each prune.

In [state.rs](../../crates/plurxd/src/state.rs),
`discover_cluster_fragment_indexes_with_permit` performs the expensive
cluster prune **before** the file loop's `pretranscode_worker_idle()` check.
If the worker is busy, the loop stops before assigning `last_examined`.
`JOB_LAST_VOD_INDEX` is stamped only when `last_examined` is set. Therefore:

```text
indexing overdue → prune → worker busy → no file examined → no stamp
        ▲                                                   │
        └──────────── next 60-second scheduler tick ─────────┘
```

The code establishes this repeat mechanism. Live settings also showed m6's
last-index stamp at Unix seconds `1790899243`, which is
**2026-10-02 00:00:43 UTC = 2026-10-01 20:00:43 EDT**, while these later
attempts ran. This corrects the earlier accidental EST conversion. The
logs show repeated cleanup-associated losses near the same second of each
minute. Other nodes have separate schedules, which can contribute additional
passes between those minute boundaries.

The exact early-exit reason for each historical pass was not logged. Do not
assign every pass specifically to `worker busy` rather than lost permit or
another zero-examination exit. The scheduling defect is that cleanup and
discovery share a completion stamp even when only cleanup ran.

## 4. Reproduction — a small index removes the measured cost

The retained [evidence receipt](../evidence/heated-rivalry-s1e5-quorum-2026-10-01.json)
contains all captured expiry indices, the production-file plan, copied-table
counts, SQLite version and compile options, SQL hash, planner statistics
counts and trial results.
The [standalone reproduction](../evidence/quorum-prune-repro-2026-10-01.py)
contains the exact deployed SQL.

The final experiment ran on nuc4 using SQLite **3.53.2**, compiled from the
amalgamation in pinned `libsqlite3-sys 0.38.1`. A private Python process loaded
that library. No daemon binary, database schema or service configuration was
changed. The retained library reports `ENABLE_STAT4`; its complete
`PRAGMA compile_options` and SHA-256 are recorded in the receipt, read back
from that retained build during review. These are reproduction-build
options, not a measurement of the running daemon's compile options.

`production_file_plan` is `EXPLAIN QUERY PLAN` executed by that reproduction
engine against the production database opened read-only. Trial `plan`
fields are generated by the same engine against the in-memory snapshot,
with the stated variant's indexes and predicates. Neither is a plan
captured from the daemon. The script emits compile options on each run so
loading `sqlite_stat4` rows cannot be mistaken for proof that STAT4 is enabled.

The script opens the source with `mode=ro` and holds a consistent read
transaction while copying these tables and their explicit indexes into
`:memory:`:

| Table | Rows |
|---|---:|
| `cluster_fragment_index_jobs` | 18,092 |
| `cluster_fragment_index_artifacts` | 6,324 |
| `cluster_fragment_index_heads` | 6,149 |
| `cluster_fragment_index_locations` | 12,788 |
| `analysis_requests` | 27,433 |
| `files` | 6,285 |

It also copies 18 `sqlite_stat1` rows and 24 `sqlite_stat4` rows for those
tables. Each measured deletion runs inside a transaction and is rolled
back. The statement uses the real 30-day retention cutoff and limit 128.

| Variant, three trials | Elapsed range | Median | Approximate VM instructions | Rows deleted |
|---|---:|---:|---:|---:|
| Existing SQL and indices | 1,221.89–1,330.43 ms | 1,257.82 ms | 52.83–52.84 million | 0 |
| Only the proposed unconditional lookup index added | 1.68–1.96 ms | 1.77 ms | 40,000–50,000 | 0 |
| Existing indices; explicit nonempty-key predicates added to both request subqueries | 2.39–2.96 ms | 2.43 ms | 70,000–80,000 | 0 |

With the proposed index, the plan changes to:

```text
SEARCH request USING COVERING INDEX repro_requests_result_target_force
  (result_cache_key=? AND target_node_id=? AND force_rebuild=?)
SEARCH active_request USING INDEX repro_requests_result_target_force
  (result_cache_key=?)
```

**How to read this:** the operation spends over a second proving it has
nothing to delete. The index removes that computation; no data removal is
needed to achieve the improvement. The explicit-predicate alternative proves
that partial-index eligibility is the relevant planner issue.

**Limits:** this is not the running daemon executable or a three-node test.
It excludes disk commits, migration/index-build cost, network round trips
and scheduler contention. Production row triggers were listed but not copied;
because all trials delete zero rows, their row bodies would not run. The
zero-row comparison establishes the cost of this incident case, not semantic
equivalence for every retention case. Functional regressions are still
required. VM counts are approximate, measured in batches of 10,000.

To repeat on an authorized node, use Python linked to SQLite 3.53.2 and pass
the database explicitly:

```bash
# From the repository: the script prints measurements, never source rows.
python3 docs/evidence/quorum-prune-repro-2026-10-01.py \
  --database /srv/plurx/hiqlite/state_machine/db/plurx.db
```

An ordinary host Python may link another SQLite version; the script refuses
that mismatch. For the retained experiment, a temporary SQLite shared
library was selected with `LD_PRELOAD` for the reproduction process only.
This does not require or justify replacing the system library.

## 5. Secondary failures — distinguish established defects from missing evidence

### 5.1 The browser guesses “ad blocker” from a failed HTTP probe

[stall-diagnosis.js](../../crates/plurxd/src/web/player/stall-diagnosis.js),
`stallDiagnose`, gives an existing server explanation priority only when
`!explained.retryable`. Otherwise it probes the URL. The final branch treats
an unsuccessful non-5xx HTTP result as evidence that the stream “looks
blocked” and supplies ad-blocker advice. A retired session returning a
4xx response can take that branch; the response alone cannot identify an
extension as the cause. `probePlaybackSource` returns status, not a parsed
typed refusal body.

The incident establishes that this advice followed known server teardown
twice. The precise probe status/body was not retained, so it is not correct
to claim a particular 404 or 410 was observed. The branch itself can be
reproduced with either response.

**Proposed correction:** preserve the latest authoritative terminal cause
for the current attachment and classify the probe's response body through
the existing failure parser. Unknown 4xx, network failures and missing
sessions need factual messages. Do not attribute them to an ad blocker
without evidence. A 5xx is not, by itself, proof that FFmpeg failed either.
Keep the attachment and request-order guards so an old response cannot
replace a new playback attempt's diagnosis. Remove the dead
`segState !== 'blocked'` condition in this same cleanup: `segState` is null
or a numeric HTTP status, so that comparison cannot classify a blocked
request.

### 5.2 Safari's error 3 does not establish a separate codec defect here

[transport.js](../../crates/plurxd/src/web/player/transport.js) sends native
media error codes 3 and 4 into the transcode fallback when real playback has
not been established. The incident error arrived after the producer was
retired. The error code establishes what Safari reported; it does not
establish that this file's HEVC bitstream is independently incompatible.

**Proposed correction:** before treating a native error as codec evidence,
consult the current attachment's known authoritative terminal cause. A
known `serving_fenced` retirement should enter the existing terminal-session
reopen path, once fresh authority permits it. Retain the genuine decoder
fallback when no contradictory server cause exists. Use the existing
recovery owner; add no new watchdog or retry controller.

### 5.3 The fallback's 409 is real, but its exact candidate rejection is unproved

[HLS creation](../../crates/plurxd/src/http/hls/create.rs) returns
`candidate_recipe_changed_or_decoder_unavailable` when candidate selection
finds no acceptable entry. The code merges several causes into this message.
The retained log does not include the fallback request body, returned
candidate catalogue or worker-refusal details. A disappearing worker during
quorum loss and a capability/recipe mismatch cannot be distinguished from
that log alone.

**Proposed correction:** retain bounded reason codes at this existing
selection boundary: catalogue unavailable because of authority, requested
identity absent, incompatible decoder, or no suitable encode route. Include
requested route/height and a bounded candidate count, without credentials
or full capability documents. A temporary worker-authority failure should
remain an availability refusal rather than claiming a permanent decoder
limitation. Reproduce the exact candidate case before changing selection.

### 5.4 The missing index prolonged exposure to the rolling path

The episode had no cluster source attestation, index artifact or fragment
build job in the inspected tables. A later inspection of **analysis
requests**, a separate pre-build layer, found a queued `fragment_index`
request for m6 with `last_error_code=foreground_preempted`. Subtitle
extraction had completed successfully on nynuc.

This corrects a possible misreading of “no index job”: a prerequisite
analysis request existed, but it had not produced a fragment-index job or
artifact. `vod_index_pending` is a prerequisite refusal, not proof that a
builder is presently running.

[VOD creation](../../crates/plurxd/src/vod/serve/create.rs) enqueues copy
preparation through
[`enqueue_copy_preparation_for_object_with_viewer`](../../crates/plurxd/src/state.rs).
Its base request uses background priority and can also join a viewer
interest. The request row's priority alone therefore does not prove the
effective scheduler priority was wrong. The observed preemption and missing
artifact warrant an acceptance case, not an unproved second root cause.

After the primary fix, verify that a new-file playback request reaches a
completed index under normal foreground load, with the correct viewer
interest and resource budget. Do not make the viewer wait for an entire
file scan before allowing the existing first-play fallback.

## 6. Proposed change — repair the access path, then its amplification

### 6.1 Required database correction

Add a schema-owned, unconditional index with a descriptive final name:

```sql
CREATE INDEX analysis_requests_result_target_force
ON analysis_requests(result_cache_key, target_node_id, force_rebuild);
```

The unconditional form preserves existing cleanup semantics for every
stored key, including empty keys. Adding `result_cache_key <> ''` directly
to the query is faster too, but changes behavior for an empty job cache key;
the production job table does not enforce a nonempty-key CHECK constraint.
Prefer the measured additive index over that implicit data assumption.

Implement it in the standalone SQLite and Hiqlite migration chains, using
the next available versions on the implementation base. Preserve upgrade
and migration-parity tests. `analysis_requests` has previously been rebuilt
through RENAME → CREATE → copy → explicit index recreation. A later rebuild
can silently discard this index. Pin its presence and the plans of both
production statements against schemas created by the **complete migration
chain on each backend**, including upgraded databases; a hand-built table
fixture cannot protect this contract. Future rebuilds must retain the index
in their explicit recreation lists. Relevant owners are
[the shared index schemas](../../crates/plurx-core/src/store/fragment_index_cluster.rs),
[SQLite initialization](../../crates/plurx-core/src/store/sqlite/mod.rs),
[replicated migration dispatch](../../crates/plurx-core/src/store/hiqlite.rs)
and [replicated index statements](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs).

Do not run ad-hoc schema DDL on an individual production voter. An index
that fixes only the leader can leave followers applying the same statement
slowly, and unmanaged DDL bypasses the repository's schema-version contract.
Measure index-build cost in upgrade testing; additive does not mean free.

Keep all retention guards: current-source failed history, forced retries,
active analysis, active jobs, heads, artifacts and locations. Add tests for
the real prune implementation rather than an independently rewritten query.

### 6.2 Bound maintenance scheduling independently of indexing progress

Separate the cleanup attempt cadence from the file-discovery progress stamp
inside the existing scheduler owner. A successful cleanup with zero deleted
rows must consume its maintenance interval even if foreground playback
prevents examining files. A skipped discovery must not claim files were
indexed or move the discovery cursor.

A low-frequency maintenance cadence can reuse the existing scheduler and
lease mechanisms. Choose and document its interval during implementation;
do not invent another periodic task. If an eligible-row read allows skipping
a no-op replicated deletion, treat it only as an optimization: the eventual
write must still recheck all guards against races.

The missing lookup index is the primary correction. Lowering frequency
alone still leaves a scheduled playback outage; a preflight query alone
does not make an expensive replicated deletion safe when it does run.

### 6.3 Preserve the cause through web recovery

Implement the corrections in sections 5.1–5.2 through the current surface,
control and reopen owners. One attachment has one authoritative retirement
cause; a later generic media error or HTTP probe cannot turn it into an
unsupported claim about an extension or codec. A successor needs its own
identity and must not inherit a predecessor's failure.

Keep candidate-selection changes limited to a reproduced defect. Recording
the bounded 409 classification is useful; selecting an arbitrary lower
candidate or bypassing decoder validation would conceal a different bug.

### 6.4 Close the diagnostic gap at the existing proof boundary

OpenRaft 0.9.25 exposes `get_read_log_id()` separately from
`wait(...).applied_index_at_least(...)`. The deployed wrapper currently calls
their combined convenience API. A follow-up can time these stages while
preserving the same absolute deadline, leadership checks and read semantics.

Record the pending read index and local applied index on a slow attempt.
Add bounded slow-apply evidence with Raft index, elapsed time and a static
operation class. Do not log arbitrary SQL bind values or replicate telemetry
for each proof. This should make a future slow statement identifiable from
ordinary diagnostics without needing the WAL before it is purged.

## 7. Validation — acceptance must exercise the failure that escaped

The existing tests
`fragment_location_retention_is_bounded_through_dyn_store` in
[store_contract.rs](../../crates/plurx-core/tests/store_contract.rs) and the
history-index plan tests in
[sqlite/fragment_index_cluster.rs](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs)
cover retention behavior and other projections. They do not establish that
this terminal-job prune is cheap with thousands of retained failed jobs.

The following tests are **proposed**, not already executed or implemented:

| Regression | Required evidence |
|---|---|
| Both exact plans with retained failures | Exercise the actual candidate SELECT and final DELETE against schemas produced by the complete SQLite and Hiqlite migration chains; each forced-request probe searches by cache key, target and force flag, rather than filtering a node-wide population |
| Zero-deletion worst case | Thousands of old failed jobs still refer to current files and have no forced request; the baseline reproduces excessive VM work, and both corrected statements remain within deterministic instruction budgets |
| Retention equivalence through both Stores | Ready/cancelled history, current and obsolete failed sources, forced requests, active requests/jobs, heads, artifacts, locations and deletion limits retain their intended behavior |
| Existing-database upgrade and rebuild retention | Fresh and historical databases traverse the full migration chain on both backends; the final schema retains the index and both production plans use it, including after table rebuilds. Measure index-build cost and preserve migration parity |
| Busy discovery scheduling | Run consecutive minute ticks while discovery is busy; no cursor progress is invented, and cleanup does not repeat on every tick |
| Quorum under cleanup | Three-node fixture runs the production prune while proof refresh and mutable playback continue; application latency stays inside budget and no serving loss is caused by pruning |
| Real partition | The existing minority-partition and stale-owner tests still fence; the performance fix must not weaken authority |
| Native error after fenced retirement | Same-attachment `serving_fenced` followed by Safari-style error 3 enters reopen recovery without codec blame; genuine decode failures still use the codec fallback |
| Probe after retirement | Exercise 404, 410, 503, typed refusal bodies, missing bodies and stale attachments through the shipped diagnosis code; no unsupported ad-blocker/FFmpeg claim |
| New episode with pending index | First play remains available, viewer demand is represented, and preparation can finish under the intended foreground budget |

For SQL tests, use instruction growth or query-plan structure rather than a
tight wall-clock assertion on a shared CI runner. For live acceptance,
measure wall-clock behavior on the fleet. The 1.77 ms local median is not a
new universal latency guarantee.

Before implementation, establish the repository-pinned Rust **1.97.1**
compiler loop. Example existing checks, followed by the new regressions:

```bash
cargo test -p plurx-core --features hiqlite-store --test store_contract \
  fragment_location_retention_is_bounded_through_dyn_store
node --test tests/playback/web-control.test.js tests/playback/web-policy.test.js
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

Use a corrective `fix(` or `perf(` subject and real `Regression-Test:` lines
for the tests implemented. Follow the affected-surface gate and current-base
requirements in [AGENTS.md](../../AGENTS.md). A proposed test name in this
document is not valid landing evidence.

For deployment acceptance, include a real cleanup pass and at least two
effective discovery intervals: the observed cadence is 16 minutes, so use
a window of **at least 33 minutes**, deliberately including a busy foreground
period and a zero-deletion pass. Verify all voters' schemas,
application/proof timings and serving-loss counters. Play this exact episode
in Safari, repeat start and seek, and confirm the new-file index reaches a
usable state. A quiet interval in which cleanup never ran proves nothing.

## 8. Boundaries and remaining uncertainty

**What is established:** eight retained quorum-loss boundaries coincide
with this next Raft entry; the exact query's work exceeds the proof budget;
one usable lookup index removes that work on the incident dataset and pinned
SQLite version. Cleanup scheduling and web diagnosis have concrete
amplifying paths in the deployed source.

**What remains to prove during implementation:** migration cost and backend
parity, preservation of nonzero-deletion semantics, real-cluster acceptance,
the exact reason for the fallback's 409, and the effective viewer-priority
path for the pending new-file index. The file's complete decode and Safari
compatibility have not been certified.

**Non-goals:** increasing the proof lease, permitting a minority to serve,
removing the apply wait from the serving proof, resurrecting a retired
generation, replacing Raft, adding a recovery
controller, rewriting the index scheduler, or redownloading/re-encoding this
episode as a purported cluster fix. None follows from the measured cause.
The correction needs no feature flag or Developer toggle. The apply-wait
coupling explains why any sufficiently slow replicated write can exhaust
the proof's remaining lease (about 500 ms in an aligned refresh cycle).
Removing that wait is a tempting workaround that changes the serving proof's
safety contract; repair slow operations while preserving the proof.

This investigation made no production schema, setting, membership or
service changes. All experimental DDL and deletions ran in disposable
in-memory databases. The requested deliverable is this diagnosis and fix
proposal; playback repair is not yet delivered.


## 9. Review disposition and implementation handoff

The 2026-10-01 review approved the mechanism and index recommendation with
changes. All requested document corrections are incorporated: raw stamp and
EDT conversion (§3.3); cadence evidence and off-phase exceptions (§2.1);
candidate-read cost and coverage (§3.1, §7); full migration-chain and rebuild
protection (§6.1, §7); reproduction-build options and plan provenance (§4).
The two permits, 33-minute acceptance window, apply-wait non-goal, and dead
web comparison are included in their relevant sections. Generated Python
bytecode is excluded from the deliverable.

Implementation progress, validation receipts and decisions belong on the
[build status page](HEATED-RIVALRY-S1E5-QUORUM-STATUS.html). The user requested
one larger PR with proper incremental commits, an adversarial review when
ready to merge, then one passing fast-lane result for each required check on
the code being merged. Rerun failed checks and checks invalidated by later
changes, preserving applicable passing evidence. Do not use the full unit
suite as an exploratory compile loop. The development pipeline's dated
amendments supersede its older automatic qualification instructions. Its
September 20 amendment still describes workspace unit tests as blocking;
if the enforced fast lane conflicts with the user's later instruction to
leave full unit failures to a separate process, surface that conflict
rather than silently weakening or bypassing the gate. Keep all work in an
independent agent clone and preserve the existing architectural owners.
