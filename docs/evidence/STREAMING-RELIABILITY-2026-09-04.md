# Streaming reliability evidence — 2026-09-04 audit snapshot

**Source baseline:** `48615baf` · **History start:** `8fae5517` · **Collection:**
read-only · **Privacy:** aggregate playback fields only; no titles, usernames,
network addresses, credentials, request bodies, or media paths retained.

Companion to
[STREAMING-RELIABILITY-REVIEW.md](../STREAMING-RELIABILITY-REVIEW.md). This is
the reproducible evidence behind the review's history, fleet, and nightly
claims. Source-code evidence remains in the review's finding index.

## History scope

Run from the isolated clone:

```bash
git rev-list --count --since='2026-08-28 00:00:00 -0400' 48615baf
git rev-list --first-parent --count \
  --since='2026-08-28 00:00:00 -0400' 48615baf
git diff --shortstat 8fae5517..48615baf
git diff --name-only 8fae5517..48615baf | wc -l
```

Recorded result:

```text
1,193 commits
217 first-parent changes
512 files changed, 220,101 insertions, 50,655 deletions
```

The playback-focused sample included the server HLS, control, transcode, VOD,
web, Apple, Android, playback-test, and playback-documentation paths. Its
`git diff --numstat` aggregate was 140 files, 103,516 insertions, and 33,872
deletions.

## Fleet build and capability snapshot

The supplied deployment key was used for read-only SSH. The build command was:

```bash
ssh -i /Users/pjunod/code/plurx-agent/.ssh-deploy-key HOST \
  'docker exec plurxd plurxd --version'
```

All four nodes (`nynuc`, `m6`, `nuc3`, `nuc4`) returned:

```text
plurxd 0.3.0 (v0.3.0-639-g03379035)
```

Containers were created between 15:32 and 15:37 UTC. Startup logs recorded
NVENC unavailable because CUDA could not load. QSV and VAAPI basic paths
validated, while Main10/HDR capability differed by node; for example, some QSV
Main10/HDR graphs failed or were explicitly refused. These are capability
observations, not playback passes.

The bounded collection command was:

```bash
ssh -i /Users/pjunod/code/plurx-agent/.ssh-deploy-key HOST \
  'docker logs plurxd 2>&1 | head -n 24'
```

The retained projection was limited to encoder capability lines. It reported
`Cannot load libcuda.so.1` for NVENC on each node, validated basic QSV and
VAAPI, and included node-specific QSV Main10/HDR rejection. Raw logs were not
committed because they contain dynamic infrastructure detail; the exact
command above reproduces the bounded view.

The queue snapshot came from the unauthenticated local metrics surface:

```bash
ssh -i /Users/pjunod/code/plurx-agent/.ssh-deploy-key nynuc \
  'curl -sS --max-time 5 http://127.0.0.1:32400/metrics | \
   grep -E "plurx_analysis_queue_depth.*component=\"fragment_index\""'
```

At collection it contained 2 claimed background jobs, 218 normal-background
and 11 forced-admin jobs in `retry_wait`, 1,141 published rows, and 3,367
failed rows across the reported fragment-index labels. These are durable state
depths, not 24-hour event counts; the repair must not present them as rates.

## Seven-day aggregate playback telemetry

The node-local query, with the same projection on each SQLite-capable host:

```sql
SELECT count(*), datetime(max(at_unix_ms) / 1000, 'unixepoch')
FROM playback_events
WHERE at_unix_ms >= (unixepoch() - 7 * 86400) * 1000;

SELECT count(*), count(DISTINCT session_id),
       round(avg(ms), 1), max(ms)
FROM playback_events
WHERE event = 'stall'
  AND at_unix_ms >= (unixepoch() - 7 * 86400) * 1000;
```

De-identified result:

| Node | Events | Stall records | Stall sessions | Mean reported ms | Maximum reported ms |
|---|---:|---:|---:|---:|---:|
| `nynuc` | 10,208 | 102 | 43 | 8,547.8 | 222,802 |
| `m6` | 21,916 | 642 | 156 | 196,469.7 | 896,194 |
| `nuc3` | 0 | 0 | 0 | — | — |
| `nuc4` | not queried | — | — | — | — |

`nuc4` lacked the host `sqlite3` executable; “not queried” must not be read as
zero. Event rows span several deployments, so this table establishes field
failure classes and scale, not regression status for build `03379035`.

A bounded detail query projected only timestamp, event, method, encoder,
height, duration, runway, reason, and the first 120 characters of diagnostic
detail. It found one `m6` VAAPI session repeatedly reporting the same frozen
position with `server_hold`, about 61.5 seconds of runway, and increasing stall
duration over many minutes. No media title or user field was retained.

Post-container-create queries on `nynuc` and `m6` contained only 66 and 65
`producer_pass` rows respectively at collection time, and no post-restart
stall. That sample is explicitly too short to prove a fix.

## Nightly validation

The exact listing command was:

```bash
gh run list --repo pjunod/plurx --workflow 'validation nightly' \
  --limit 8 --json databaseId,conclusion,createdAt,headSha,url
```

Every returned run concluded `failure`:

| UTC date | Run |
|---|---:|
| 2026-09-04 | 33851587914 |
| 2026-09-03 | 33731767479 |
| 2026-09-02 | 33606292748 |
| 2026-09-01 | 33488599729 |
| 2026-08-31 | 33379820140 |
| 2026-08-30 | 33303757213 |
| 2026-08-29 | 33247259399 |
| 2026-08-28 | 33185304765 |

The latest run's playback-smoke and playback-exhaustive steps failed before a
case started with `Chrome not found`. The workflow action had installed
Playwright Chromium, but the Make targets passed no executable to the lab. The
same wiring fault affected the inspected window; later repair evidence must
record both a green workflow and positive proof that playback cases started.

## Existing VOD acceptance receipts

The failing evidence is already retained in repository handoffs:

- [VOD-STEADY-ACCEPTANCE-HANDOFF.md](../VOD-STEADY-ACCEPTANCE-HANDOFF.md)
  records run 32943477931 / job 98105071882: four visible hitches, a cold-start
  `bufferStalledError`, 0.165× presentation with 21.3 seconds buffered, and
  serving-authority expiry. Its exact focused rerun command is retained there.
- [VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md](../VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md)
  records run 32929741906 / job 98059637890: all 20 seeks used one session,
  but nine visible hitches, about 0.1 seconds of runway, and authority expiry.
- [VOD-STALL-ACCEPTANCE-HANDOFF.md](../VOD-STALL-ACCEPTANCE-HANDOFF.md)
  records run 32918531993 / job 98032989197: the shaped case timed out before
  first frame after Auto selected a VOD transcode rung that the server refused.

Those handoffs contain some now-stale architectural prose about the live engine;
this evidence file retains only their run IDs, observed results, and commands.

## Pull-request review evidence

PR [#903](https://github.com/pjunod/plurx/pull/903) used three independent
read-only audits (server, clients, operations/architecture) followed by a
fresh exact-head adversarial review. Each documentation commit ran the effort
fast lane: history audit, validation catalog, 160 operations tests, rustfmt,
and pinned Rust 1.97.1 workspace/all-target compilation. The remote Effort
development gate passed on `d438731b`; subsequent review-fix commits must carry
their own green gate and exact-head re-review.
