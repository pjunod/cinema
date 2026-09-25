# Fleet readout — current-main evidence and collection windows

**Status:** preliminary · **Build:** `f600d28230222005441cfc62301c306785c852ce` · **Observed:** 2026-09-25 02:24–02:33 UTC

This appendix records read-only evidence for [K-02](../cluster/RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md), [C-05](../server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md), [C-08](../server/OBSERVABILITY-BASELINE.md), [P-02](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md), and [S-11](../streaming/CODEC-AND-GPU-QUALIFICATION.md). It supplements the [deployment record](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md) on the integration branch. The four current nodes are `nynuc` (192.168.5.236), `m6` (192.168.4.14), `nuc4` (192.168.4.8), and learner `nuc3` (192.168.4.7); older plan aliases are not additional machines.

## Collection — a bounded history, not a completed window

At 02:28:44 UTC, direct `/metrics` scrapes returned HTTP 200 from all four nodes with `plurx_build_info{build="v0.3.0-3881-gf600d2823"}`. A local collector under `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/` now requests each endpoint every 30 seconds. It retains selected metric lines in daily JSONL files, not the approximately 326 KiB full exposition:

| Window | Selected families | Purpose |
|---|---|---|
| First hour, every five minutes | `plurx_http_requests_total`, `plurx_http_body_seconds_*`, `plurx_http_bodies_total` | C-08 RED baseline after an hour of use. |
| First 24 hours, every 30 seconds | `plurx_raft_snapshot_seconds_*`, commit/applied indexes, apply lag, four state-machine byte gauges, backfill counters | K-02 B/E/S/W and C-05 convergence history. |
| Seven days, every 30 seconds | build, uptime, encoder availability, accepted sessions and tone-map sessions | S-11 reset-aware counter deltas. |

The collector uses a seven-day wall-clock deadline, one file per UTC day, and a 256 MiB total-size stop. From measured selected-row sizes, the projected seven-day total is about 219 MiB; the cap wins if series grow. The collection source, `window.json`, and daily files stay outside the repository. A previous macOS LaunchAgent attempt could not route to the nodes and was removed with its error samples; the active approved shell session returned HTTP 200. Check for gaps, build changes, and uptime resets before computing any delta. A missing interval is incomplete evidence, never a zero.

No 24-hour or seven-day result exists yet. No voter was restarted, so K-02's applied-index catch-up rate A remains owed. The session and its local files may not survive host shutdown; the final readout must verify the timestamps rather than assume continuity.

## C-05 — current marker population is converged

`VALIDATION_REVISION` is 1 in [segplan.rs](../../crates/plurx-core/src/segplan.rs). At 02:29:55–02:30:14 UTC, read-only queries of `/srv/plurx/hiqlite/telemetry.db` reported sidecar schema v10 on every node:

| Node | `fragment_indexes` rows | `ABS(validated_revision) < 1` | Positive markers | Negative markers |
|---|---:|---:|---:|---:|
| `nynuc` | 2,239 | 0 | 2,239 | 0 |
| `m6` | 1,505 | 0 | 1,505 | 0 |
| `nuc4` | 4,251 | 0 | 4,251 | 0 |
| `nuc3` | 0 | 0 | 0 | 0 |

The query was `PRAGMA user_version; SELECT COUNT(*), COALESCE(SUM(ABS(validated_revision)<1),0), COALESCE(SUM(validated_revision>0),0), COALESCE(SUM(validated_revision<0),0) FROM fragment_indexes;` through `sqlite3 -readonly` on `nynuc`, `m6`, and `nuc3`; `nuc4` has no `sqlite3` CLI, so Python's `sqlite3.connect("file:/srv/plurx/hiqlite/telemetry.db?mode=ro", uri=True)` ran the same SQL. All four `plurx_index_validation_backfill_total` result counters were zero at 02:28:44 UTC. The present library-bearing sidecars have no pending validation markers. These observations do not prove that the bounded backfill loop processed legacy rows after this deploy; there may have been none. C-05's M2/M3 rollout and availability behavior remain separate acceptance work.

## P-02 — the sample was idle

At 02:27:36–02:27:40 UTC, the read-only command below ran on each host; the full labelled output is `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/process-limits-20260925.txt`.

```bash
ssh -i ~/code/plurx-agent/.ssh-deploy-key pjunod@<node> \
  "docker exec plurxd sh -c 'grep -i \"open files\" /proc/1/limits; ls /proc/1/fd | wc -l; cat /proc/1/oom_score_adj; cat /sys/fs/cgroup/pids.current /sys/fs/cgroup/pids.max'; docker stats --no-stream plurxd"
```

| Node | Open-file soft/hard | FD count | OOM adjustment | Cgroup PIDs current/max | Docker CPU and memory |
|---|---:|---:|---:|---:|---|
| `nynuc` | 524,288 / 524,288 | 61 | 0 | 33 / 74,782 | 12.27% · 1.166 GiB |
| `m6` | 524,288 / 524,288 | 56 | 0 | 30 / 29,428 | 13.72% · 1.284 GiB |
| `nuc4` | 524,288 / 524,288 | 57 | 0 | 35 / 37,300 | 32.75% · 1.724 GiB |
| `nuc3` | 524,288 / 524,288 | 47 | 0 | 28 / 37,262 | 3.50% · 491 MiB |

`plurx_transcode_sessions_active`, active Live TV sessions, and recording consumers were all zero. The image lacks `pgrep`, so that subcommand returned `not found`; it did not supply an FFmpeg PID list. No workload was started. The plan requires at least two transcodes, one direct play, and one DVR recording on `media1` and `lab1` during the busy sample; this idle snapshot does not satisfy it.

## K-02, C-08, and S-11 — starting points only

The 02:28:44 UTC Raft gauge baseline was:

| Node | DB bytes | WAL bytes | Snapshot bytes | Log bytes | Apply lag |
|---|---:|---:|---:|---:|---:|
| `nynuc` | 170,762,240 | 11,029,272 | 165,531,685 | 33,554,489 | 0 |
| `m6` | 167,047,168 | 9,352,432 | 165,535,781 | 16,777,273 | 0 |
| `nuc4` | 169,193,472 | 2,323,712 | 165,535,781 | 33,554,489 | 0 |
| `nuc3` | 166,944,768 | 14,848,512 | 165,535,781 | 33,554,489 | 0 |

The collector now includes histogram buckets, counts and sums, so a complete 24-hour run can compute build count and p50/p99. One gauge reading cannot establish whether lag exceeded 64 between scrapes; the interval and all build/uptime continuity checks must be reported with the result.

At 02:32:58 UTC, `nynuc` had one QSV/SDR accepted session since its current process started; the other sampled encoder-session and tone-map counters were zero. Those are process-local starting counts, not seven-day usage or absence evidence. The C-08 one-hour RED series remains in progress. JSON-mode log verification, browsing, and media-body flow in its plan require separate actions and are not claimed here.
