# Fleet readout — current-main evidence and collection windows

**Status:** first-hour readout, mixed builds · **Starting build:** `f600d28230222005441cfc62301c306785c852ce` · **Observed:** 2026-09-25 02:24–03:29 UTC

This appendix records read-only evidence for [K-02](../cluster/RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md), [C-05](../server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md), [C-08](../server/OBSERVABILITY-BASELINE.md), [P-02](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md), and [S-11](../streaming/CODEC-AND-GPU-QUALIFICATION.md). It supplements the [deployment record](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md) on the integration branch. The four current nodes are `nynuc` (192.168.5.236), `m6` (192.168.4.14), `nuc4` (192.168.4.8), and learner `nuc3` (192.168.4.7); older plan aliases are not additional machines.

## Collection — a bounded history, not a completed window

At 02:28:44 UTC, direct `/metrics` scrapes returned HTTP 200 from all four nodes with `plurx_build_info{build="v0.3.0-3881-gf600d2823"}`. A local collector under `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/` now requests each endpoint every 30 seconds. It retains selected metric lines in daily JSONL files, not the approximately 326 KiB full exposition:

| Window | Selected families | Purpose |
|---|---|---|
| First hour, every five minutes | `plurx_http_requests_total`, `plurx_http_body_seconds_*`, `plurx_http_bodies_total` | C-08 RED baseline after an hour of use. |
| First 24 hours, every 30 seconds | `plurx_raft_snapshot_seconds_*`, commit/applied indexes, apply lag, four state-machine byte gauges, backfill counters | K-02 B/E/S/W and C-05 convergence history. |
| Seven days, every 30 seconds | build, uptime, encoder availability, accepted sessions and tone-map sessions | S-11 reset-aware counter deltas. |

The collector uses a seven-day wall-clock deadline, one file per UTC day, and a 256 MiB total-size stop. From measured selected-row sizes, the projected seven-day total is about 219 MiB; the cap wins if series grow. The collection source, `window.json`, and daily files stay outside the repository. A previous macOS LaunchAgent attempt could not route to the nodes and was removed with its error samples; the active approved shell session returned HTTP 200. Check for gaps, build changes, and uptime resets before computing any delta. A missing interval is incomplete evidence, never a zero.

The initial selector needed a histogram-suffix correction. Complete K-02 histogram-family samples begin at 02:31:51 UTC; the 24-hour collection deadline was extended ten minutes so that window can fill. At 02:34:28 UTC, the first 48 stored node samples had HTTP 200 and zero request errors. This early clean segment does not imply the later window is gap-free.

No 24-hour or seven-day result exists yet. No voter was restarted, so K-02's applied-index catch-up rate A remains owed. The session and its local files may not survive host shutdown; the final readout must verify the timestamps rather than assume continuity.

## First-hour C-08 readout — mixed-build, interrupted window

The collector's first-hour file at
`/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/first-hour-result.json`
contains 436 node samples (109 per node) from 02:29:14 to 03:28:41 UTC. Every
node has a 337-second gap from 02:43:34 to 02:49:11 when the collector process
ended and was restarted. `nuc3` refused connections at 03:25:11 and 03:25:41
while new main was deployed. The build gauge changed from `f600d2823` to
`dafadf043` on `nynuc` at 03:22:11 and on `nuc3` by 03:26:11; `m6` and `nuc4`
still reported `f600d2823` at 03:28:41. This is not a continuous, single-build
hour of normal use, so C-08's full-hour acceptance remains owed.

After the hour, five direct `/metrics` scrapes per node all returned HTTP 200.
The slowest of those five was 0.053 s (`nynuc`), 0.123 s (`m6`), 0.042 s (`nuc4`),
and 0.127 s (`nuc3`). None of the 20 scrapes contained a UUID-like route value,
`token`, `/mnt/`, or `file_id=` under the specific hygiene check. The plan's
literal grep also matches the word `session` in ordinary metric names: 535 hits
per node across five scrapes. That literal check is not clean. JSON-mode restart,
media-body flow, and browsing remain owed. The interrupted/mixed-build window
also cannot satisfy K-02's 24-hour or S-11's seven-day continuity requirements.

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

At 02:32:58 UTC, `nynuc` had one QSV/SDR accepted session since its current process started; the other sampled encoder-session and tone-map counters were zero. Those are process-local starting counts, not seven-day usage or absence evidence. The C-08 first hour is described above as an interrupted, mixed-build readout. JSON-mode log verification, browsing, and media-body flow in its plan require separate actions and are not claimed here.

## Post-promotion observation window — current main

The serial rollout of [PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) finished on all four nodes. Independent verification at 05:14:47–49 UTC found exact main `44cdfccc7`, healthy containers, zero restarts, `/readyz` 200 and the matching `plurx_build_info` line on each. The [deployment receipt](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md#13-post-promotion-four-node-rollout--2026-09-25) has per-node image identities. The collector's first complete four-node HTTP-200 tick on that build was **2026-09-25 05:14:11 UTC**, followed by another at 05:14:41 UTC. This is the start of the new single-build observation; the earlier 337-second collector gap, mixed builds and deployment refusals stay in the file and do not count toward it.

K-02's 24-hour series from this start is due no earlier than 2026-09-26 05:14:11 UTC. S-11's seven-day series is due no earlier than 2026-10-02 05:14:11 UTC. The original collector was started at 02:28:44 UTC and its in-memory K-02 and seven-day deadlines fall short of these new ends. It continues unchanged. An overlapping K-02 collector began at 05:17:35 UTC in `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/k02-20260925.jsonl.gz`: its first four-node tick returned HTTP 200, 87 selected metric lines per node and the exact current build. It runs every 30 seconds through 2026-09-26 05:15:11 UTC with a 32 MiB supplement cap and 256 MiB combined cap. The primary collector covers the 05:14:11–05:17:35 overlap. The seven-day extension still requires a supervised successor near its original deadline; the final audit must join timestamps and prove no gap, uptime reset or build change. C-08's new one-hour normal-use readout is due after 06:14:11 UTC and still needs the named active browser/body/JSON checks. None of these windows is complete at this update.

## Uniform deployed-build C-08 hour — 05:14–06:14 UTC

A subsequent continuous hour on build `v0.3.0-3974-g44cdfccc7` yielded 122 30-second samples on each of four nodes (488 total), all HTTP 200 and exact-build, with no uptime reset and no gap over 30 seconds. Five direct post-hour `/metrics` scrapes on each node were also HTTP 200; the largest was 326,792 bytes and the slowest 0.135 seconds, within the plan's 2 MB and 500 ms limits. The four-node readout is stored locally at `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/c08-exact-main-hour-readout-20260925.md` (SHA-256 `9d6a3d7dd868f262261b70fe3939f1e02bc9f41a9cdb719fc91511800221835b`); raw JSON SHA-256 `627b741e88409b3abe2f04866c0bc3712fab60357342ddcfe70f1a2058da465b`.

A Chrome session on m6 browsed six app sections, rendered a VOD first frame, sent two forward-seek inputs and closed. The completed-body counter increased from 4 to 134; no seek-accuracy claim follows from this automation. Normal and deliberately malformed `x-request-id` input each produced a generated 32-hex response ID. The last 200 Docker log lines per node had no ANSI escape prefixes. A narrower label-value hygiene check found no UUID-like route values, tokens, file IDs or mount paths in the 20 direct responses; the plan's literal grep still matches 107 metric-name lines containing `session` per scrape. JSON log mode requires a restart and remains unverified while the K-02 and S-11 continuity windows run. C-08 acceptance is partial.

## Current-main Apple Release — 12:39 UTC

After evidence PR #512 merged as `e2dfc6b77aa965e039b111038bb15a6ca352aab1`, a signed iOS and tvOS Release build from that exact source completed with Team `YHK542LK23`; codesign verification passed. CoreDevice independently reported `tv.plurx.app` version `0.3.0`, build `183` on 17air, 17promax, Bedroom Apple TV and iPad Pro after installation. 16pro, iPad Mini and iPhone 18 Pro were unavailable at inventory and after install. The source and installed bundle receipts are local: `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/apple-pr512-release-20260925.log` (SHA-256 `e72f94605e77c34d4ffca7aab48162572b0b8119f4df7509252b2c7d33c5dbef`) and `apple-pr512-device-verification-20260925.json` (SHA-256 `eec9332f6747423a328bf71d65ecece9264bc02e26a421d0c26e5e24c4128ced`). Bundle metadata does not expose source SHA, so the source claim comes from the verified release build and install log. No controller, paging, caption or shaped-network interaction acceptance was observed on these devices.

## Current-main three-voter rollout — 12:55 UTC

A serial Ansible run of merged main `e2dfc6b77aa965e039b111038bb15a6ca352aab1` on `nynuc`, `m6` and `nuc4` completed with zero failed or unreachable hosts. The log is `/Users/pjunod/code/plurx-agent/codex-postmerge-node-deploy-e2dfc6b77-20260925.log` (SHA-256 `140f00fb00e57e4660f48c82f408d785d5d0adb5dfd73e345c48b6dcf6984892`). Independent SSH checks after the run found that all three had the exact source checkout and OCI `org.opencontainers.image.revision` label, reported build `v0.3.0-4072-ge2dfc6b77`, Docker `healthy` with zero restarts, and returned `/readyz` HTTP 200.

`nuc3` is not part of that success. At 12:55 UTC its checkout was still `363d48e78`, its container image was `8251d14f75a5`, Docker reported `unhealthy` with two restarts, and `/readyz` reset the connection. Another interactive process had an older `363d48e78` Docker build active on that host for about 48 minutes. A new rollout was withheld to avoid concurrent Compose operations. The earlier K-02 and S-11 single-build windows are interrupted by this rollout; a four-node current-build window cannot start until `nuc3` is recovered and independently verified.
