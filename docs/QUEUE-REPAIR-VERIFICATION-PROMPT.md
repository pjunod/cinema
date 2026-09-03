# Queue repair verification — did the index queue come back, and stay back

You are verifying, on the live fleet, that the fragment-index queue is
building again after the repair effort and the sampled-attestation change.
Everything below is a read. **Do not change code, do not restart nodes to
"help", and do not retarget or edit queue rows.** If a gate is shut, stop and
report which one — a shut gate is the answer, not an obstacle.

Two efforts land together and this verifies both:

- `effort/fragment-index-queue-repair` (PR #881) — the queue can be watched,
  reopened, and stops building the same file once per voter.
- `agent/sampled-source-attestation` (PR #877) — attestation reads a bounded
  64 MiB sample instead of hashing whole films.

## 0. What you have

- SSH to the four nodes as `pjunod@{nynuc,m6,nuc4,nuc3}` with the key at
  `~/code/plurx-agent/.ssh-deploy-key` (copy to `~/.ssh/id_ed25519`, chmod
  600).
- The plurx API needs a bearer token. Get one the way a browser does, or ask
  Paul. Unauthenticated `curl localhost:32400/api/v1/system` returns
  `{"error":"authentication required"}`.
- `/metrics` is unauthenticated.

## 1. Check you are testing the build you think you are

Before anything else, on each node:

```bash
curl -s localhost:32400/metrics | grep -m1 plurx_build_info
systemctl show -p ExecStart plurx 2>/dev/null | head -1
```

The running binary must contain both merges. If any node is behind, say which
and stop — a fleet running two different queue policies is not a measurement,
and it is exactly the shape that made the last verification unreadable.

## 2. The one-word answer, on every node

This is new and it is the fastest read in this document:

```bash
for h in nynuc m6 nuc4 nuc3; do
  echo "== $h"
  ssh pjunod@$h 'curl -s localhost:32400/metrics | grep -E "^plurx_analysis_queue_"'
done
```

Report the verdict per node and the six figures beside it.

**How to read it.** `plurx_analysis_queue_health{verdict="…"} 1` is the
answer; exactly one series is 1. The verdict is **fleet-wide**, not per node —
`cluster_fragment_index_jobs` is replicated and records no observer, so every
node computes the same answer from the same rows. Nodes disagreeing is itself
a finding: it means they are not seeing the same store, and that is a
replication problem, not a queue problem.

| verdict | what it means here |
|---|---|
| `healthy` | Indexes were built in the last 24 h and most claimed work finished. This is the target. |
| `idle` | Nothing waiting, nothing picked up, nothing built. After a deploy that reopened work, this is **wrong** — it means discovery is not requesting. Check `vod_index_mins` is not `0` and that `playback.vod_index_cluster_cache` is on. |
| `degraded` | Producing, but losing leases or exhausting retry budgets. Read `plurx_analysis_queue_attempt_limit_24h` against `plurx_analysis_queue_claimed_24h`. |
| `dead` | Nothing built in 24 h. The two halves point different ways — see below. |

If `dead`: compare `plurx_analysis_queue_claimed_24h` and
`plurx_analysis_queue_claimable`. **Jobs claimed and none finished** is a
worker or source fault; **a backlog with zero claims** means no node is
picking work up at all. Report which.

If the whole `plurx_analysis_queue_*` family is **absent** rather than zero,
the store sampler has not completed a cycle. Check
`plurx_store_metrics_sample_valid` and `plurx_store_metrics_sample_errors_total`
and report those instead — absent is not the same as idle and must not be
reported as such.

## 3. Is it actually producing

```bash
ssh pjunod@nynuc 'sudo cp /srv/plurx/hiqlite/state_machine/db/plurx.db* /tmp/ && \
  sudo sqlite3 /tmp/plurx.db "
    select datetime(max(built_at_ms)/1000,\"unixepoch\") from cluster_fragment_index_artifacts;
    select pipeline_sha256, count(*) from cluster_fragment_index_artifacts group by 1;
    select state, last_error_code, count(*) from cluster_fragment_index_jobs group by 1,2;
  "'
```

Copying the db, `-wal` and `-shm` to `/tmp` first is the polite way to read a
live WAL-mode file.

**Baseline at 03:03 UTC 2026-09-03:** 1,939 `attempt_limit` · 145
`source_attestation_timeout` · 851 of 5,847 files indexed · last artifact
2026-08-31 19:19:44 · **zero** converting-pipeline artifacts.

**What to look for.**

- `max(built_at_ms)` advancing past 2026-08-31 is the first proof of life.
- A **second** `pipeline_sha256` appearing is the converting pipeline being
  built for the first time ever. That is the headline result for Dolby Vision
  Profile 7, and it may not appear at all — see §6.
- `attempt_limit` will **not** shrink on its own. Those rows are history and
  nothing deletes them. §5 is how they come back.

## 4. Did attestation actually get cheaper

The claim is that attestation now costs about 64 MiB regardless of file size,
so it should no longer be the thing that times out.

```bash
ssh pjunod@nynuc 'journalctl -u plurx --since "2 hours ago" | grep -iE "attest|verifying" | tail -40'
curl -s localhost:32400/api/v1/analysis/jobs?filter=working -H "Authorization: Bearer $TOKEN" | jq '.rows[] | {title, phase, bytes_read, total_bytes}'
```

**What to look for.**

- A row in the `verifying` phase should show **non-zero** `bytes_read` and a
  `total_bytes` of about 67,108,864 — not the file's own size. Before this
  change the phase published three zeros, which is why the daemon's real hash
  rate had never been measured.
- `source_attestation_timeout` should not be growing. Check the count in §3
  twice, an hour apart.
- If it **is** growing, that now means a genuinely hung mount rather than a
  large file: the read it bounds is 64 MiB. Report which node and which
  library path.

## 5. Bringing the stranded rows back

The 1,939 `attempt_limit` rows are terminal, and terminal is a durable
statement. Nothing reopens them on its own. **Preview first — an empty body is
a dry run and changes nothing:**

```bash
curl -s -X POST localhost:32400/api/v1/analysis/reopen \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{}' | jq
```

Report `reopened`, `files`, `skipped_unavailable`, `scan_truncated` and
`stopped_at_headroom` before doing anything else. **Do not run with
`dry_run: false` unless Paul says to** — it puts real work on a queue that has
just come back, and the point of the preview is that he sees the number first.

If he does say to, `limit` counts distinct **files**, ceiling 500 per call.
Run it, wait, read §2 again, and only then run it a second time. Reading the
verdict between calls is the entire reason for the ceiling.

**"Nothing left to reopen" is the healthy answer**, not an error: a file whose
successor is already queued or built is deliberately not offered again.

## 6. What is NOT fixed, so do not chase it

**No converting Dolby Vision index will appear from the background pass**, and
that is expected. Background discovery issues one request per file, that
request resolves the *first* identity lacking an index, and the request row is
then the dedup tombstone — so a DV file gets its stripped identity and never
its converting one. Only a play attempt or an admin request can ask for the
converting identity today.

That is milestone M5.2 and it is **not built**: it needs a column on
`analysis_requests`, which is a schema bump on both backends and therefore a
stop-the-fleet deploy. The analysis is in
`docs/CONTENT-ANALYSIS-INDEX-HANDOFF.md` §5.9 and the decision is Paul's.

So: if you want to see a converting artifact appear, ask for one explicitly on
a known P7 title (file 70) rather than waiting for the background pass. If it
still does not appear after that, *that* is a finding worth reporting.

Also expected and not a fault: `foreground_preempted` continuing to appear on
busy nodes, and `attempt_limit` staying flat at 1,939 until §5 is run.

## 7. Rate — read it, do not tune it

`INDEX_MAX_PER_PASS = 4` per voter on a fifteen-minute `vod_index_mins` means
working through 5,847 files takes roughly two weeks, not hours. Both halves of
the old bottleneck have just moved — the voters stop duplicating each other's
builds, and attestation stopped costing a whole-file hash — so any number you
measure in the first hour is measuring the transient.

**Do not raise `INDEX_MAX_PER_PASS` or `MAX_REQUESTS_PER_PASS.`** Report
`plurx_analysis_queue_ready_24h` against `plurx_analysis_queue_claimable`
after 24 hours and let Paul decide.

## 8. Report

One short section per numbered step, saying what you ran and what came back.
Name the node for anything node-specific. If you stopped at a shut gate, say
which gate and what it was shut on — that is a complete answer.
