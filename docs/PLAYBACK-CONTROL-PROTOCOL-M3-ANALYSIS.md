# Playback control protocol M3: analysis control and status

This slice implements the operator-facing foundation from
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) §6.6 on
top of the clustered fragment-index queue. It adds a durable **Analyze now /\
Rebuild analysis** request, a bounded admin API, and a dedicated web status
page without changing playback selection or removing a watchdog.

The existing `POST /items/{id}/reanalyze` remains an immediate ffprobe repair.
It is not renamed, redirected, or treated as an index request.

## Operator flow

An administrator can request analysis from each available video file on the
item page. The button commits an `analysis_requests` row before hashing the
source or starting ffmpeg, then opens **Analysis status**. The same status page
is linked from Activity.

The status page polls the read-only `GET /api/v1/analysis/jobs` snapshot and
shows:

- queued source inspection and currently leased source hashing;
- queued and running fragment-index jobs, including background work;
- the target or owner node, lease expiry, attempt count, pipeline version,
  request trigger, age, and typed terminal error;
- completed work and the title/file it belongs to; and
- whether analysis is paused by the existing cluster-index-cache rollout gate.

The page reads replicated summaries only. Polling it does not renew a lease,
settle a job, or write to Raft. A worker performs terminal request settlement
on its existing bounded cadence.

## Two-stage durable queue

A fragment-index cache key includes the complete source SHA-256. An HTTP
handler therefore cannot enqueue the existing content-addressed job without
either blocking on a complete media read or acknowledging work that exists
only in memory. `analysis_requests` is the durable stage before that identity
is known:

```text
button -> analysis request -> source fence/hash -> fragment-index job -> artifact
           replicated          node-local I/O      replicated          node-local bytes
```

The request records file size and modification time at admission. A leased
worker reopens that exact scanner identity, renews its claim during hashing,
and records the complete digest only while the descriptor remains unchanged.
It then derives the existing deterministic pipeline/cache identity and submits
the fenced fragment-index job. The request transition and worker-job
insert/reopen are one SQLite or Raft transaction: either both become visible or
neither does. Every claim mutation requires the exact request id, node, attempt
fence, source generation, and unexpired lease.

Hashing is cooperatively cancellable throughout the complete media read. Live
playback demand or disabling the rollout gate yields the request back to the
queue without consuming an attempt; claim loss performs no terminal write.
I/O and replicated-store failures use typed, delayed retries and consume the
bounded attempt budget. The status page keeps the retry code and next eligible
time visible.

Concurrent ordinary requests for one source generation/component/node join the
active row. If a scanner update changes size or modification time, a trigger
cancels the stale active generation and the replacement generation can be
admitted immediately under the same file id. Once hashing proves that a
replacement file has the same content/pipeline key, it may join the existing
ready artifact even when scanner file id, size/mtime generation, or path
changed. A canceled terminal worker row is rebound with a fresh queue age; the
published artifact remains available throughout.
A force request does not silently replace an ordinary request already in
flight; it returns conflict and can be made once the first generation
finishes. Likewise, force does not delete the published artifact. It reopens a
terminal worker row while the existing artifact and installed structural index
continue serving. Only the newly verified deterministic result closes the
successor.

Foreground media demand still preempts analysis. A button press wakes the
bounded worker immediately, while the ordinary scheduler remains the recovery
path after restart.

## API

Both routes require an administrator:

```text
POST /api/v1/files/{file}/analysis
     { "force": false, "components": ["fragment_index"] }

GET  /api/v1/analysis/jobs
```

The POST returns `202 Accepted` only after the request is durable. It reports
whether the request joined identical active work. The status response is
bounded to 500 requests, 500 worker jobs, and 500 file/title labels. It omits
source digests and filesystem paths; pipeline identity is shortened for
display.

The queue is initially governed by the existing default-off **Cluster index
cache** setting. When paused, POST returns conflict with exact remediation and
the status page remains readable. Existing published indexes continue serving.

## Storage and migration

SQLite schema v31 and replicated schema v13 add only the request table and its
indexes/cleanup triggers. A partial unique index admits one active request for
a source generation/component/target node. Deleting a source cancels its
queued, running, or submitted request without deleting a previously published
artifact.

The existing worker rows now expose their bounded typed failure code through
the store projection. New list methods keep operator reads bounded and fetch
file/title labels from bounded request/job inputs in one joined query rather
than an Activity-page N+1. Terminal request history is retained for 30 days and
capped at the 20 newest generations per file/component/target. Pruning is
bounded to 256 rows and locally throttled to no more than once per hour.
An update trigger independently enforces a hard global ceiling of 8,192
terminal requests, so the hourly pass is age/per-file cleanup rather than the
only growth bound.

## What this slice does not claim

- The packed `FragmentIndex` is still the structural, video-only component.
  Intro, recap, credits, and preview annotations remain a separate replicated
  timeline-annotation component of the logical analysis index.
- `skip_markers`, detector feature sidecars, retry/cancel endpoints, exact
  byte/media-time progress, and fleet metrics remain follow-up work.
- A forced request targets the ingress node in this slice. Background
  discovery and the resolved content-addressed worker remain cluster-fenced;
  cross-node request placement is not yet advertised.
- No control action, automatic quality change, stream handoff, subtitle
  generation, playback recovery, or legacy watchdog behavior changes here.

These boundaries are visible rather than implicit: unsupported components are
rejected, the status page says when analysis is paused, and the documentation
does not describe structural indexing as semantic marker detection.

## Rollback

Turn off **Cluster index cache**. New requests are refused and queued work is
left durable but unclaimed. Published structural indexes remain installed and
playback continues to use the existing VOD/live-recovery decision. Rolling the
binary back leaves additive request rows inert; it does not invalidate the
older fragment-index artifact schema.
