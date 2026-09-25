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

## Partial current-main passive evidence — three voters

A synchronized 12:56 UTC read-only snapshot on `nynuc`, `m6` and `nuc4` found each exact `e2dfc6b77` image healthy, restart-free and `/readyz` 200. Exposition bodies were 393,467, 393,390 and 392,811 bytes; Raft apply lag was zero on each. C-05 sidecars reported zero pending with 2,258, 1,509 and 4,287 positive rows. Open-file limits remained 524,288 and current file descriptors were 51, 50 and 55. S-11 accepted encoder and tone-map counters were zero during this idle snapshot; that does not prove absence across a window. C-08 M5 families were present: 28 TTFF count series, four each for seeks, stalled seconds, watched seconds and delivered bytes, and three admission-wait series. No active observation occurred, so seek-to-picture lacked a count series. Planned start-outcome and scratch-byte families remain unbuilt.

The local raw receipt is `/Users/pjunod/code/plurx-agent/codex-owed-evidence-receipts-20260925/three-voter-e2df-20260925T1258Z/receipt.json` (SHA-256 `e4b381ecc706f49a14102f341f27440595f755216132c799cc4d46e80297624e`) and summary JSON SHA-256 `1a47d3131c88f6336439d22b1fb328bdd538c6633aabaf3d96b93281a377fc67`. A privacy-preserving nynuc L-01 guide-window receipt shows 90,000 seconds (25 hours), SHA-256 `773029ed20f826849360397d87dcce8f9fe43fe1ba28d992802736398b91e34f`. This is three-voter passive baseline evidence; it does not complete a four-node or active-flow plan prompt.

## A-04 current-main 350 kb/s browser trace

A locally stamped `plurxd --version` reported `v0.3.0-4072-ge2dfc6b77` before an exact-main Chrome VOD trace. The baseline was 0.99997×. The network stage held 350 kb/s for 74.834 seconds with measured media throughput 349.3 kb/s. Recovery failed: there was no continuous 10 seconds at 0.9× speed, one restart/downshift occurred at 21.548 seconds, maximum video gap was 16.433 seconds, with seven waits, three stalls, ten hitches and zero runway. The raw trace is `/Users/pjunod/code/plurx-agent/codex-a04-evidence-20260925/web-chrome-e2df-8-to-350-stamped.json` (SHA-256 `fb58040bf99b3759f2af27b182a2ed2cc04dc43d5cafdf3ebe3a218cafc0cdd6`); normalized SHA-256 `00d4bd9a59966883b571dc8dde5754cb010b62616a6b84a8ad51792d597f7d6b`; receipt SHA-256 `34d865e8e91de0bdef9adb03fad60d11275ca1cf10587f4f48592733c777c2ea`.

The browser `playback-lab run` observer initially applied and scored only its first cliff. Commit `36b8f06a5` first made it refuse unsupported multistage browser profiles. Draft PR #516 commits `196997b65` and `781223a21` now apply and score both 75-second cliffs and reject a source shorter than the 177-second two-window minimum. An exploratory Chrome run against exact-main `e2dfc6b77` applied 8→1.1 Mb/s at 19.198 seconds and 1.1→350 kb/s at 94.204 seconds, with measured stage rates 1099.2 and 349.2 kb/s. Recovery at the first cliff failed: no sustained 10 seconds at 0.9×, one restart, first downshift at 21.318 seconds, an 8.6-second maximum video gap, eight waits, one stall and 15 hitches. The second stage began from a 0.747× baseline, so it does not independently prove second-cliff recovery. Raw report `/Users/pjunod/code/plurx-agent/codex-a04-multistage-evidence-20260925/codex-a04-multistage-trace.json` SHA-256 `c0e11b12365bcc3276fa98a738ad148f0574797b0b4974ae961262e9e7701509`; normalized SHA-256 `064ec0f75824730666cc29b6cdd8476d2a8a6c421aa044c08bcefef3bb783ba1`. Safari WebDriver session creation timed out and Firefox/geckodriver were unavailable. These are failed Chrome baselines, not A-04 D3 acceptance.

## Nuc3 forensic learner inspection — 13:14 UTC

The old unhealthy learner and its competing Compose operation were stopped without modifying the data directory. The original `/srv/plurx` remains intact; a mode-700 copy at `/srv/plurx.forensic-20260925T131450Z` was inspected read-only using the stopped image and `plurx-cluster-check inspect-wal`. Metadata CRC was valid. The retained WAL starts at index 20,745,454 while `last_purged` is 20,750,863 and the snapshot index is 20,750,864; the inspector verdict is `metadata_ahead_of_wal`. The report is `/Users/pjunod/code/plurx-agent/codex-nuc3-incident-evidence-20260925/nuc3-wal-inspection-20260925T131450Z.json` (SHA-256 `0531738625437b4f925dc0985a94a345c12fccef7768d06d2350d837423c2282`). The documented recovery is to keep this data stopped and rejoin a clean learner from the healthy three-voter quorum with authenticated cluster administration. No raw metadata/WAL edit or attempted uncredentialed removal was made. The first exact-current-main four-node tick, and therefore new K-02/S-11 continuous windows, remain owed.

## Additional passive row context — 12:56 UTC

The same three-voter `e2dfc6b77` receipt supplies deployment context for S-01, S-02, S-03, K-04, C-07 and L-02, whose active prompts remain unrun. The sampled S-03 journals had zero `dv_proof_failure`, `subtitle_empty_stderr` and `media_origin_no_answer` matches (combined SHA-256 `fd852dd8b39bc327d27990341d0eeb00bd619e9517d82ce74a166ad8255038c4`); an idle zero is not a controlled-workload pass. K-06 `timedatectl` offsets were +970 µs on nynuc (raw SHA-256 `72688c0eae59faa9a79d1e4b25680a128553abec7046aa0e3017748e123c6989`), −15 µs on m6 (`e3c1a582234debde0039653a7ef4c461704671479ddb4d658e59369fe786ad46`) and −830 µs on nuc4 (`b5f785c348597042707fb731fdd64d4b9a470aa85fa8281ecf38f24d9a402cd9`). These are point readings, with no runtime clock metric, one-hour series or learner sample. Shared receipt SHA-256 `e4b381ecc706f49a14102f341f27440595f755216132c799cc4d46e80297624e`.

## Broken legacy collection closed — 13:27 UTC

After `nuc3` stopped and the four-node single-build window became impossible, only the verified old collector processes were stopped. The primary raw file is 41,614,975 bytes with 5,212 rows, SHA-256 `b685b934a5531761859c59903b7bccbe1fddf9b42fe39278f1cf7f0c618599d8`; the K-02 supplement is 184,073 bytes compressed with 700 rows and a verified gzip stream, SHA-256 `a861589f8f4df5750fb3516b42d38c4fd77186e5a523a9952eb08217d65e412c`. The primary has a 337-second gap per node; the supplement has a 24,076-second gap per node. Neither qualifies as a continuous 24-hour or seven-day result. Exact-process stop receipt `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/legacy-collector-stop-20260925.json` SHA-256 `65bd4403223d3d72d8ea168c826aebdea76110830fc24c98bbe6094d010ca7d0`. A compressed restartable collector is prepared but inactive until four nodes share one verified build.

## Physical interaction revisit — 13:28 UTC

The sanitized receipt at `/Users/pjunod/code/plurx-agent/codex-physical-acceptance-20260925/receipt.json` (SHA-256 `d2791c2eef76d9bcc4915e71ccee0bad0dc1e1c30df044785236cdcc06062217`) records four paired Apple devices with Plurx 0.3.0 build 183 installed. Bedroom Apple TV refused foreground launch because the system was asleep; 17promax showed a lock screen, 17air was asleep, and iPad Pro failed CoreDevice remote XPC. Four connected Android devices were locked and still ran debuggable 0.3.0 versionCode 124, not signed release 125. No device was unlocked or reinstalled in this revisit, and no D-02/A-02/A-03/L-03 interaction pass was inferred. Transient lock-screen captures were discarded.

## Exact newer-main Apple Release install — 13:34 UTC

A fresh agent-owned clone at `b47c5ff88867201e595a7511fe49ef606e07307e` built iOS and tvOS Release; both signatures verified Team `YHK542LK23`, bundle `tv.plurx.app` 0.3.0 build 183. Forced CoreDevice reinstall succeeded on 17air, 17promax, Bedroom Apple TV, iPad Pro and iPhone 18 Pro, with post-install bundle/version/build queries on each. 16pro and iPad Mini were unavailable. The build number did not change, so device metadata alone cannot identify source SHA; the exact-source build log and successful forced-install receipt provide the source link. Receipt `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/apple-b47-device-install-20260925.json` SHA-256 `209b564d3ce08c072f51c506dad2f7e15393be52678fd2084e7beca7e66234b`; build log SHA-256 `925d70bb5c43ba93b2b3b5718d1937ab15a2d0eead4f6d7475849a299a56cf18`. Archived signed iOS/tvOS bundles have SHA-256 `0032570f90a03d6658d8407bc600f3eaf8b24f84ba9e673a224480af4a30585f` and `00d5bf6f58f3ac4b4f9cde22f9a461a3a44143455c151398c03cf32def61432d`. These installs do not turn the earlier sleep/lock physical interaction revisit into an acceptance pass.

## Nuc3 unexpected old-image restart — 13:32 UTC

A delayed container created by the earlier Compose operation started from image revision `363d48e78` against original `/srv/plurx` while the host checkout pointed at `b47c5ff88`. It briefly reported Docker healthy, `/readyz` 200 and local Raft apply lag zero. This does not cancel the stopped-copy `metadata_ahead_of_wal` forensic verdict or satisfy exact-main deployment. The bounded startup/process log is `/Users/pjunod/code/plurx-agent/codex-nuc3-incident-evidence-20260925/nuc3-unexpected-restart-20260925T1334Z.log` (SHA-256 `aa40368084407b63b17e2841eefdb5e167f8703d26cd4b72ae6eb9fcbcd6be1f`). That exact old-image learner was stopped again and remained exited after a three-second check; original and forensic data were preserved. A new owner-only `/srv/plurx-rejoin-20260925` contains only the copied configuration and awaits authenticated membership removal and a fresh join token.

## Newer-main three-voter rollout — 13:49 UTC

After #510 advanced main to `b47c5ff88867201e595a7511fe49ef606e07307e`, the serial deployment independently verified `nynuc`, `m6` and `nuc4`: each checkout and running OCI revision matched exact main; Docker reported healthy and zero restarts, `/readyz` returned 200 and the build gauge was `v0.3.0-4087-gb47c5ff88`. At 13:49:27 UTC each reported three fresh voters, zero stale voters, quorum available, leader nuc4 and zero Raft apply lag. Verification log `/Users/pjunod/code/plurx-agent/codex-main-b47-healthy-voters-verification-20260925.log` SHA-256 `32699c5e494add8a60b231e33b81bb4f7c708f73949d73f5b38a6640e118ad8b`; quorum log SHA-256 `41ba8767281381713c49e4d957ac865829b0e96c8929b41918b1f706e67c97ae`. The authenticated m6 Cluster page independently showed build convergence at b47, three ready voters, term 18070 and maximum lag zero.

The first playbook exited 2 on a transient m6 Compose container-name race (log SHA-256 `8dd1ed1891ac1507faa6fd6f77f61dacac1403a8b4b4faa9cae591b8b85da9ba`); direct checks confirmed m6 nevertheless ran the correct healthy image. An unforced nuc4 run exited 0 but left its previous e2df image running because its checkout had already advanced (log SHA-256 `ddbf4143fcd8094deb500255474c823b1aab4a2d13655ea2106aea55f1c8663f`). A forced nuc4 run exited 0 and replaced it (log SHA-256 `037205dfaa2708fe70e3eac2456f5d49984a2898fcd865ec9dffdd0479521996`). Nuc3 was not included. Three-voter success is not a four-node deployment or a new K-02/S-11 continuity window.

## Synchronized current-main passive baseline — 13:49:52 UTC

The b47 three-voter read-only sample spanned 0.585 seconds. Each node had exact checkout and OCI revision `b47c5ff88`, Docker healthy/restarts zero, `/readyz` 200 and Raft apply lag zero. The private receipt is `/Users/pjunod/code/plurx-agent/codex-owed-evidence-receipts-20260925/three-voter-b47-20260925T134952Z/receipt.json` (SHA-256 `5a863aa7226ceeac8f7d21bd426604f7fbad18e477e0994545c98c3c69248239`); summary SHA-256 `019523a4471c9973f912eab9ccf19adcf95bfdc834cc2005c7b3129081acedeb`, with 13 constituent file hashes verified. Nuc3 was excluded.

| Row | Three-voter point observation | Acceptance still owed |
|---|---|---|
| K-02 | Nuc4 leader, term 18070, zero apply lag; B/E/S/W size gauges and snapshot counts present. | Gap-free 24-hour series and approved follower restart/catch-up. |
| C-05 | Sidecar positive rows 2,260 / 1,515 / 4,291; pending zero and backfill counters zero. | Active postdeploy convergence/availability behavior. |
| C-08 | Bodies 392,832–393,661 bytes, scrape 0.059–0.073 s, M5 families present; m6 has process-local nonzero TTFF, watched, stalled, delivered and admission counters. | New four-node normal-use hour, JSON mode and controlled active-flow checks. |
| P-02 | Open-file soft/hard limits 524,288; file descriptors 56 / 76 / 50. | Busy sample and week without EMFILE. |
| K-06 | `timedatectl` NTP offsets −977 / −120 / −63 µs; no `plurx_cluster_clock` metric. | Runtime clock metric and one-hour idle/loaded peer uncertainty. |
| S-11 | QSV available 1 / 0 / 1; NVENC and VideoToolbox zero. Accepted encoder counters 3 / 0 / 0; tone-map zero. | Seven reset-aware days and controlled usage, with build/uptime continuity. |

These are point readings. Nonzero process-local counters have no controlled start/end boundary, and zero counters do not prove absence across a duration.

## L-03 current-main Chrome caption baseline — 14:00 UTC

An isolated Chrome session displayed server build `v0.3.0-4087-gb47c5ff88` and played 6.1 WTVR-HD at 1920×1080 H264/AAC for 86 seconds. At media times 12.3, 42.6 and 85.9 seconds, `#live-tv-video.textTracks` held one `{kind: captions, label: English 708, language: en, mode: hidden, cues: null}` track. The picture inspected near 42 seconds showed no caption text; Playback info said Subtitles Off, and no caption control was visible. The agent stopped the stream and verified `paused=true`, `readyState=0`, empty `src` and `currentTime=0`. Local receipt `/Users/pjunod/code/plurx-agent/codex-l03-caption-evidence-20260925/l03-b47-chrome-6-1-20260925.json` has SHA-256 `90d7e32ad640fec0b5955abaa62210102e3414ec93a1c60a12a0c669a66067f7`.

Earlier source proof on 6.1 at `44cdfccc7` recorded CC1/SERVICE1, receipt SHA-256 `40d52c38f861c064b370c5f0e6eebb87192b338d259a4726d50bbd079237825a`. Because that proof was neither concurrent nor on the same build, the b47 browser observation cannot distinguish absent live source captions from client rendering failure. L-03 M4 web and physical caption acceptance remain open.

## PR #516 promotion audit correction — 14:09 UTC

Fast lane #3000 on ready head `2ad0f194d` stopped in `make history-check` before compile jobs. The audit found that prior merged PR #512 (`e2dfc6b77`) carried corrective commit `1831ee898` without the required `Regression-Test:` trailer in its immutable landing message. Its retained regression is `tests/operations/test_live_tv_caption_audit.py::test_audit_requires_one_completed_test`; #512 fast lane #2976 had passed before merge. The append-only `validation/merge-errata.toml` records this specific historical omission. A local `make history-check` completed successfully after the erratum; a new exact-head PR fast lane remains required.

## L-03 concurrent source, HLS and web caption check — 14:15 UTC

The same active 6.1 session on exact running b47 nynuc was checked at three points. A fresh 25-second HDHomeRun source capture (27,039,388 bytes, SHA-256 `014d73cad3e37a2fd686fc3a8590e5d167da8fe47916844bcb3bdd6026c559d4`) decoded seven CC1 cues. Nine server H.264/AAC HLS TS segments numbered 236–244 from that session (18,992,512 bytes, SHA-256 `821468b5980bcaca545fae710d99b103b4c2ee65b774b54e937581925ec8e897`) decoded five CC1 cues. Chrome played past 113 seconds with the advertised `English 708` track hidden, cues null, Playback info `Subtitles: Off`, and `#pbsubs` set to `display:none`; no caption text was drawn. The browser stream was stopped and all four tuner slots returned idle. Receipt `/Users/pjunod/code/plurx-agent/codex-a04-evidence-20260925/l03-channel-6-1-b47-web-versus-encoded-20260925.json` SHA-256 `6598d7839cf9bf4ee9bf6267d333fa03e5070452ff93cfce527d35e0356adc35`. Temporary raw and SRT files were removed. This proves CC1 bytes in the source and deployed encoded segments, but does not independently prove 708 output or complete the M4 web/device caption pass.

## PR #516 focused A-04 validation — 14:18 UTC

During the post-review fast-lane phase, `node --test tests/playback/network-shaping.test.js` first found one stale assertion expecting the old unnumbered cliff error. The test now expects the numbered `cliff 1` fault emitted by the two-cliff scorer. A focused rerun passed all 98 shaping contracts (10.997 seconds); the local log is `/private/tmp/plurx-pr516-network-shaping-final.log`, SHA-256 `3d134c95f02516e06c685ac55eea8899eab4c5ce7641fbd85f7e31b437fca4fd`. The ready PR needs a new exact-head fast lane after this correction.

## Merged-main Apple Release install — 14:51 UTC

An agent-owned exact-source checkout at merge `37baf6e0b509dc7adff882d1a40cc70c8a8538fd` built signed iOS and tvOS Release 0.3.0 build 183. `codesign --verify --deep --strict` passed with Team `YHK542LK23`. Forced CoreDevice install and post-install bundle queries succeeded on 17air, 17promax, Bedroom Apple TV, iPad Pro and iPhone 18 Pro; before/after installation URL hashes changed on all five despite the unchanged build number. 16pro and iPad Mini were unavailable. Sanitized receipt `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/apple-37baf6-20260925/receipt.json` SHA-256 `4d4c9ee5518241b39ab042b7035797cdda6854107b0056489ab30777430040e1`; signed archives and build logs are stored beside it. This is exact-source install evidence, not a controller, paging or caption interaction pass.

## Merged-main Apple interaction revisit — 14:56 UTC

On the already-unlocked iPhone 18 Pro, CoreDevice launched the signed Plurx executable and identified its exact PID; the immediate screen showed its startup spinner, then Curator Discovery was foreground about six seconds later. No Plurx controller, paging or caption control was available for an acceptance check. The agent terminated only that test PID, verified it was absent, and left Curator foreground. Other iPhones and iPad Pro required passcodes; Bedroom Apple TV was unlocked but had no established CoreDevice input path. Local screenshots were inspected and removed. Sanitized receipt `/Users/pjunod/code/plurx-agent/codex-fleet-observation-20260925/apple-37baf6-20260925/interaction-check.json` SHA-256 `9ebf1669e6c940ad23dade96a8d7ce76b6ca16b2e0e4cacb2735b0c926fe8df8`. A-02, A-03 and L-03 physical acceptance remain open.

## Exact merged-main A-04 Chrome two-cliff trace — 15:00 UTC

An isolated server built from merged main `37baf6e0b509dc7adff882d1a40cc70c8a8538fd` under pinned Rust 1.97.1 reported `v0.3.0-4117-g37baf6e0b`, matching independently verified nynuc. The source-exact local binary SHA-256 was `3c9d6152cf8e6b75f16c6a5587220d5fc11828397f7dd5c249ebfeece1c24618`. Chrome's fixed two-cliff harness measured 7,998.5 kb/s for 11.987 s, then 1,099.2 kb/s for 74.948 s, then 349.2 kb/s for 74.736 s, with no transport errors. The first cliff failed: recovery took 61.748 s versus the 10 s limit, with one restart, 3.006 s maximum video gap, nine waits, two stalls and 19 hitches. The second physical cliff ran, but the fixed sampled 10 s tail before it averaged only 0.883×, below the 0.90× valid baseline, so its recovery was marked `browser_playback` rather than accepted.

Raw trace SHA-256 `a3bb6d003e81b382bbfa273822bee0f06fbf75f8988a9f7f8a02a470c4d043be`; normalized SHA-256 `b709f8f4e0e6946585ea94e863440f089bf11332ccca0eb4df815c021c4943b4`; receipt `/Users/pjunod/code/plurx-agent/codex-a04-evidence-20260925/exact-main-37baf6e0b5/receipt.json` SHA-256 `e948519f7d40d6506b5e9273e33f8d5fce9a51f9803e7a503dbf1bca86034c89`. The isolated server, Chrome listener and temporary fixture/source checkout were cleaned up. This is a failed Chrome baseline, not D3 or physical-device acceptance.

## Android physical inventory — 14:58 UTC

A read-only ADB pass saw five transports representing four physical devices: TCL 9445X had USB and Wi-Fi transports, plus Pixel 10 Pro Fold, Pixel 11 Pro XL and Motorola razr ultra 2025. All four still had `tv.plurx.app` 0.3.0 versionCode 124, `DEBUGGABLE`, with the same signing certificate SHA-256 `5cefd0c7db3f0a8d6fd818937425b7647b3222f9ed1419d79883a12c3e168bce`. The 9445X was awake and unlocked with Plurx Home foreground; a passive screen check showed populated Continue watching, Next up and Recently added rails. The other three were locked and dozing. No playback, paging, filter, notification or Home interaction was performed. The screenshot was discarded; its hash is in the sanitized receipt. No application or device state changed.

Receipt `/private/tmp/plurx-android-physical-inventory-20260925.json` SHA-256 `d31372ba321ae493c790f4b507b0c0b6e3c91ef6ada1ddb6b0647d6638eb228a`. This proves physical availability and an old debug baseline only. Current-main signed release build 125 and D-02/A-03 device acceptance remain owed; the established release signing identity and four `PLURX_ANDROID_*` inputs were not found.
