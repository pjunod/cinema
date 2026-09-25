# Architecture review fleet evidence — what the 2026-09-24 rollout proves

**Status:** evidence-only snapshot · **Source:** [architecture-review workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) at `f600d2823` · **Observed:** 2026-09-24

This record separates the four-node deployment and short, read-only observations from the longer fleet and physical-device acceptance owed by the plans. It was assembled in a fresh Forgejo clone and copied deployment playbook. No plan owner or landed milestone claim changed. At this snapshot no client was installed, no active playback was started, and no disruptive drill was run.

## 1. Deployment — four inventory nodes run `f600d2823`

The current `noirr` Ansible group contains `nynuc`, `m6`, `nuc4`, and `nuc3`. The names `media1` and `lab*` in older plan prompts are not extra hosts in that inventory; map each plan's role to the present fleet before running its prompt. A read of `origin/main` at 23:39 UTC still returned `f600d28230222005441cfc62301c306785c852ce`.

The copied playbook ran with `sync=false`, `only=plurx`, and `--limit nuc4,nuc3`; it did not synchronize or deploy the operator's other application checkouts. `nynuc` and `m6` already ran the target. The first `nuc3` attempt built the exact image under Rust 1.97.1 but failed at Compose recreation while another on-node deploy was running. After that process ended, one `build=false` retry passed container health, `/readyz`, and the playbook's durable-store activation log check and wrote `.ansible-deployed-ref`. The failed first attempt is part of the record; the retry is the verified result.

| Node | Previous running build and image, where changed | Final image SHA-256 | Final binary, health, `/readyz` |
|---|---|---|---|
| `nynuc` | Already `f600d2823` | `81fb79d4ba8ade261f1b12abd32074f1bd82a97a13634784d8c334dbce653ac9` | `v0.3.0-3881-gf600d2823` · healthy · 200 |
| `m6` | Already `f600d2823` | `7b2a58db14b0af7db2b796bc2a23f6459b32107ab27f816b3f8dd28c2a72e325` | `v0.3.0-3881-gf600d2823` · healthy · 200 |
| `nuc4` | `v0.3.0-3803-g936157b4b` · `c30011e3c8e41b1dfe18dfc0ba0160b27a1be54f298db5c7c58d09e578114d34` | `19025f0e38565d3e7ac2de19e318d17fa1dba2186e0e70159423a31bfb5818f5` | `v0.3.0-3881-gf600d2823` · healthy · 200 |
| `nuc3` | `v0.3.0-3853-gb1de09647` · `c4534110c68baea0b6de20ef73d863ea490f7c32eae1cd502242f1c5387f8d00` | `2b3e98bd34b2f0824d669221b37c9f5df04c53a04eed7497f7e599502899d6ad` | `v0.3.0-3881-gf600d2823` · healthy · 200 |

All four had Docker restart count zero at 23:36:14–16 UTC. Both `plurxd` and `plurx-discovery` were healthy on `nuc3`. Its `/api/v1/server` returns 503 with `learner_route_ineligible` because it is a non-voting learner; `/readyz` is 200. A second bounded 30-second log window at 23:38:06–07 UTC had zero `ERROR` lines on every node. That is a short post-deploy observation, not a soak or unit-test result.

## 1.1 Physical Apple install — 2026-09-25

At 01:48 UTC `origin/main` was still exact `f600d28230222005441cfc62301c306785c852ce`. From this agent's own clone, `scripts/ship-physical --apple` built signed iOS and tvOS Release artifacts with the configured development team, verified both signatures, and installed plurx 0.3.0, Apple build 182, on every reachable physical Apple device in CoreDevice's inventory: `17air`, `17promax`, `Bedroom` Apple TV 4K, `iPad Mini`, `iPad Pro`, and `iPhone`. The first run also attempted shutdown simulators because CoreDevice reported their tunnel as `disconnected`; the script now filters on `reality=physical` (`df62a015b`), and the clean rerun installed all six with exit 0. `16pro` was unavailable and received no install. The local clean-run log is `/Users/pjunod/code/plurx-agent/codex-apple-deploy-20260925-clean.log`, SHA-256 `ab5f5ebab63560a7ed666aa626eb856d568f40977762837ee5f4283900240b4c`.

This proves installation of current `main`, not any plan's playback/device acceptance. Android release remains uninstalled: the current session has none of the four release-signing inputs, a filename search in the likely local signing locations found only `~/.android/debug.keystore`, and ADB at 02:01:42 UTC found four attached Android devices: TCL 9445X, Pixel 10 Pro Fold, Pixel 11 Pro XL, and Motorola razr ultra 2025. Each runs Cinema 0.3.0, versionCode 124, with `DEBUGGABLE` set. A debug build is not substituted for the required signed release; Xiaomi and Lenovo remained absent.

### 1.2 Post-promotion Apple install — 2026-09-25

After [PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) merged as `44cdfccc7`, `scripts/ship-physical --apple` built and verified signed iOS and tvOS Release artifacts from that exact main commit. The command exited 0 and installed Plurx 0.3.0, build 183, on all six reachable physical Apple devices: `17air`, `17promax`, Bedroom Apple TV 4K, iPad Mini, iPad Pro M4 and iPhone 18 Pro. Independent `devicectl device info apps` reads confirmed `tv.plurx.app` build 183 on all six. Bedroom Apple TV, iPad Pro and iPhone launched the app. The other three rejected launch with `Locked` or `RequestDenied`; `16pro` was unavailable. The signed build and install log is `/Users/pjunod/code/plurx-agent/codex-postmerge-apple-deploy-20260925.log`. An app launch is not visual playback, remote-control, library-paging, caption or adaptive-quality acceptance, so A-02, A-03, L-03 and A-04 keep those device checks open.

## 2. Read-only measurements — useful baselines, not acceptance

All reads used the running `f600d2823` image and local `/metrics`. They establish that the instrumentation is present and provide a dated starting point. Counters reset on restart, so a zero near deploy says nothing about a week of use.

| Workboard row | 2026-09-24 observation | Why acceptance remains open |
|---|---|---|
| `C-08` | Five scrapes per node at 23:36:14–16 UTC were 325,838–326,692 bytes and 0.000978–0.002168 s; `x-request-id` appeared on all four; recent Docker logs had zero ANSI lines. A malformed ID on `nynuc` was replaced by a fresh 32-character hex ID and `passwd` appeared in zero following log lines. The RED counter exposed 315 series. | The plan asks for an hour of normal use, JSON-mode restart, browsing, and media-body flow. Its literal label grep matched 82 ordinary metric names containing `session` on each node; excluding that broad word yielded zero matches for UUID-like route values, `token`, `/mnt/`, or `file_id=` on `nynuc`. Do not report the literal grep as empty. |
| `K-02` | At 23:36:14–16 UTC, all four `plurx_raft_state_machine_bytes` families were present and `plurx_raft_apply_lag_entries` was 0 on each node. | The 24-hour every-voter B/E/S/W series and approved follower restart/catch-up observation are still owed. |
| `S-04` | Font attestation spawn and stat counts were zero on every node. | No text-burn session ran; cold/warm and per-segment measurements remain. |
| `S-11` | Software encoder available on all four; QSV on `nynuc`, `nuc4`, `nuc3`; VAAPI on all four; NVENC and VideoToolbox on none. Accepted-session and tone-map counters were zero after recent restarts. | Seven gap-free, reset-aware days on every node remain. |
| `K-06` | `timedatectl show -p NTPSynchronized` reported `yes` on all four at 23:31:28–29 UTC. | `chronyc` is absent on `nuc3` and no `plurx_cluster_clock_` metric exists in this build. NTP status is not the plan's offset and uncertainty measurement. |
| `C-05` | `plurx_index_validation_backfill_total` was zero for `validated`, `refused`, and `gone` on all four at 23:38:57–58 UTC. | A counter snapshot does not establish backfill convergence on an active library. |
| `P-02` | `/proc/1/limits` in `plurxd` showed 524288 soft and hard open-file limits on every node at 23:38:37–38 UTC; no EMFILE appeared in the preceding five minutes. | Busy-evening sample and a week without EMFILE remain. |
| `D-03` | Physical inventory showed connected Apple TV 4K, iPhones and iPad, plus Pixel 11 Pro XL, Motorola razr ultra 2025, TCL 9445X, Google TV Streamer and Pixel 10 Pro Fold through `adb`. | Required Xiaomi 25019PNF3C was absent. The Apple team was found and six reachable physical Apple devices received exact main on 2026-09-25 (§1.1). The four Android release-signing inputs remained absent, so no release APK was installed. The separate mobile-release role change is on draft [PR #23](https://github.com/pjunod/ansible/pull/23); the four attached Android devices still carry debuggable versionCode 124. |

Three client-side updates were reported by the coordinating session on
2026-09-24. They are branch work and trace evidence, not merged fleet or device
acceptance:

| Row | Current observation | Remaining |
|---|---|---|
| `A-02` | Commit `f30ef466b` implements the §5.5 `Session` credential lock. `make apple-build` passed on iOS and tvOS. | `make apple-test` has not run for this commit; other A-02 milestones and device evidence remain. |
| `W-02` | Commit `2a6fbc855` repairs playback-lab readiness, allowing a Chrome shaped-network trace to reach playback. | The §5.4–5.5 splits, type baseline, browser checks and physical input observations remain. |
| `A-04` | Exact-main Chrome 8→1.5 Mb/s rerun failed recovery: 24.974 s, one 720p→360p restart/downshift at 21.198 s, 1.883 s maximum video gap, three waits, one stall, five hitches. Measured post-cliff media throughput was 1,499.2 kb/s. Two-cliff Chrome attempts timed out before first frame. Raw reports are preserved under `/Users/pjunod/code/plurx-agent/codex-a04-evidence-20260925/`. | Safari remote automation unavailable; Firefox WebDriver profile creation failed. Apple TV asleep and iPhone locked refused native launches. Android and HDR runs, and the full six-metric matrix, remain. |

The Chrome raw report (`web-chrome-after-readiness.json`) has SHA-256
`8e01fdc99ec54f2d0a2a9a8be86dc0fe77afade3d3727b1cd09b1361a7f3a7cf`;
the normalized report has SHA-256
`7ce160648a2f9e450fd260ec82abb246af695eac22eaf678768aa8ffa38eee49`.
These artifacts are local to the observing host and are not part of this
documentation commit.

### 2.1 A-04 first-cliff trace on the PR #506 candidate

The exact PR #506 candidate server was built with pinned Rust 1.97.1 and used by `scripts/playback-lab run --suite stall-recovery --browser chrome --network-profile 8mbps-to-1.1mbps-to-350kbps@12`. The run reached a first frame and the first cliff. The 1,100 kb/s stage measured 1,099.3 kb/s media throughput over 74.873 seconds. Recovery took 20.9 seconds against a 10-second limit, with one automatic restart, first downshift at 20.833 seconds, an 8.0996-second maximum transition video gap, eight wait events, fifteen hitches and zero seconds of downshift runway. The result failed. The second 350 kb/s stage has `entered_at_ms: null` and zero bytes, so this run provides no second-cliff result. Raw and normalized reports are `/Users/pjunod/code/plurx-agent/codex-a04-evidence-20260925/web-chrome-two-cliff-pr506.json` (SHA-256 `474b2b18554eb91254e5f10c9521af14b3dd05d59ce1b2488af8705b782d0f7d`) and the matching `.normalized.json` (SHA-256 `bcf809cd920a3d613ce11f15606e250b9059cb0a1a240fcef2052edb18812613`). The platform matrix and second cliff remain owed.

### 2.2 L-03 broadcast caption service capture — 2026-09-25

At about 02:08 UTC, `nynuc` reached the HDHomeRun FLEX 4K at
`192.168.5.191`. The exact deployed `f600d2823` checkout ran
`scripts/live-tv-caption-audit --image plurx/plurxd:latest analyze` on
three 30-second broadcast captures. `/tuners.html` showed all four tuners
idle before and after. All temporary capture directories were deleted.

| Channel | Capture bytes | Frames with CC | 608 CC1 chars | 608 CC3 chars | 708 SERVICE1 chars |
|---|---:|---:|---:|---:|---:|
| 6.1 WTVR-HD | 22,641,928 | 887 | 373 | 757 | 394 |
| 6.2 CBS6XTR | 8,885,968 | 841 | 373 | 18 | 402 |
| 12.2 12 MeTV | 5,459,664 | 875 | 229 | 0 | 254 |

Each capture probed as `mpeg2video,tt,30000/1001,`; CC1 and 708 SERVICE1
carried actual dialogue on all three. SERVICE2–6 had zero text. Seven other
five-second subchannel samples (23.4, 28.1, 30.1, 45.1, 48.1, 53.1 and
65.6) were MPEG-2; no H.264 broadcast was found in that bounded sample.
This establishes service IDs for M4 on the sampled channels. Prompt C was
partly captured at 02:14–02:17 UTC on exact deployed `f600d2823`:
Chrome on macOS played channel 6.1 from `m6` at 1920×1080 H.264/AAC through
Intel QuickSync. Playback info showed `Subtitles: Off`. The active video
exposed one English caption track in `hidden` mode with no active cues, and
no caption text was drawn during more than 30 seconds. The plan's literal
`document.querySelector("video")` expression returned `[]` because the first
video element was inactive; the active second element supplied the track
observation. Playback stopped and all four tuner slots were idle afterward.
Safari, physical Apple and Android caption-menu rows, plus post-advertising
playback, remain owed. ADB found no connected Shield or Android TV; the
three attached phones reported caption setting `null` (unset), which does
not prove enabled or disabled.

The Raft size-gauge values at 23:36 UTC, in bytes, are kept here so later readings have a comparison point:

| Node | db | wal | snapshots | logs |
|---|---:|---:|---:|---:|
| `nynuc` | 170659840 | 4939912 | 165400613 | 33554489 |
| `m6` | 165871616 | 14621912 | 165404709 | 16777273 |
| `nuc4` | 169033728 | 12018072 | 165404709 | 16777273 |
| `nuc3` | 165597184 | 2624472 | 165404709 | 16777273 |

The deployment and metric observations used these read-only checks on each node:

```bash
# Confirm the running binary and readiness, not just the checkout.
git -C /opt/noirr/plurx rev-parse HEAD
docker exec plurxd plurxd --version
docker inspect --format '{{.Image}} {{.State.Health.Status}} {{.RestartCount}}' plurxd
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:32400/readyz

# Read one metrics snapshot and the open-file limit.
curl -fsS http://127.0.0.1:32400/metrics
docker exec plurxd grep 'Max open files' /proc/1/limits
```

The 2026-09-25 [fleet readout](ARCHITECTURE-REVIEW-FLEET-READOUT-2026-09-25.md)
adds C-05 marker counts, P-02 idle process limits and starting K-02/C-08/S-11
metric samples. Its bounded 24-hour and seven-day collectors are in progress;
none of those windows is complete yet.

The 2026-09-25 [fleet/client baselines appendix](ARCHITECTURE-REVIEW-FLEET-CLIENT-BASELINES-2026-09-25.md)
records bounded read-only observations for L-01, L-02, C-03, S-04, S-05
and K-06. Its 401 responses and idle counters do not satisfy the plans'
active or authenticated acceptance checks.

## 3. Remaining fleet and device evidence — one row per workboard plan

`No acceptance run` means the plan's named evidence is still owed. `Preliminary snapshot` refers only to §2; it does not change a plan's `merged` or `blocked` status. Long-duration collection, physical playback, NAS failure injection, follower restarts, and log-mode restarts were not performed in this pass.

| Row | 2026-09-24 result | Still owed |
|---|---|---|
| `S-01` | No acceptance run | M4 deployed-image §5.4 fleet check. |
| `S-02` | No acceptance run | One-week media1 prerequisite; post-deploy §6 telemetry and packet rate. |
| `S-03` | No acceptance run | Controlled M0 before-counts, then M4 fleet traces after deployed hold/release. |
| `S-04` | Preliminary snapshot in §2; acceptance open | Real text-burn session; cold/warm and per-segment attestation measurements. |
| `S-05` | No acceptance run | Deployed text-burn/remux environment, first/second TTFF, unexpected progress-key journal. |
| `S-06` | No acceptance run | Reserved n2 read-write M3/M4 rate-control captures. |
| `S-07` | No acceptance run | lab4 10/50/90 captures/histograms/Harbor Lights stills; media1 pipeline verdict; Apple TV/Chrome playback. |
| `S-08` | No acceptance run | media1 QSV/VAAPI idet, signalstats and wall-time qualification. |
| `S-09` | No acceptance run | Per-layout audio matrices; Apple TV/AVR/AirPods, Android and browser passes. |
| `S-10` | No acceptance run | SPS per family and cadence, named-device SDR CODECS, transcode/copy/remux/index peaks, Apple panel comparison. |
| `S-11` | Preliminary snapshot in §2; acceptance open | Seven gap-free reset-aware days on all four nodes; recent zero session counts are not absence evidence. |
| `S-12` | No acceptance run | ffmpeg 6/8 boxes, per-family quality/init/restart and client playback; Paul's option decision. |
| `S-13` | No acceptance run | media1 static-binary cold/warm decode-gate timing; lab2 deliberate ffprobe-change log/counter. |
| `S-14` | No acceptance run | Post-merge decomposition qualification; no fleet-specific measurement is named in board. |
| `K-01` | No acceptance run | Qualified image smoke and lab1-lab4/NAS loss/restore drills with measured RPO/RTO. |
| `K-02` | Preliminary snapshot in §2; acceptance open | 24-hour every-voter B/E/S/W series and an approved follower restart/catch-up rate. |
| `K-04` | No acceptance run | lab1-lab3 per-route bounded-read readout. |
| `K-06` | Preliminary snapshot in §2; acceptance open | Runtime measurement release, one-hour idle/loaded clock upper bounds; `chronyc` absent on nuc3. |
| `K-07` | No acceptance run | First ten post-merge lane-cost receipts. |
| `K-08` | No acceptance run | M0 timings, provider assertion/three-voter proof, corpus equivalence, conditional M5 and upstream links. |
| `C-01` | No acceptance run | Lab soak, HAR/waterfall and device reader. |
| `C-02` | No acceptance run | NAS timing and provider-drop acceptance. |
| `C-03` | No acceptance run | lab1 image concurrency/CPU/503, Chrome cold/warm bytes, Apple/Android TV browse. |
| `C-04` | No acceptance run | Three-node logout fence/failure injection, trusted proxy, real-client revocation recovery. |
| `C-05` | Preliminary snapshot in §2; acceptance open | Backfill-convergence receipt over time on an active library; M2/M3 follow only after it. |
| `C-06` | No acceptance run | Slow-sidecar real playback and three restart timings. |
| `C-07` | No acceptance run | Seven gap-free days from each node plus four concurrent ingress playbacks/seeks. |
| `C-08` | Preliminary snapshot in §2; acceptance open | One-hour normal-use scrape series; literal label regex has metric-name false positives; JSON mode restart, browser RED sanity and media-body flow remain. |
| `L-01` | No acceptance run | media1 336-hour guide vs 2 MiB clip; mixed NAS/local sink interruption and metrics. |
| `L-02` | No acceptance run | Three §6.3 fleet prompts: settings failover budget, peer resolution, and start/cleanup observations; M3/M4 implementation also pending. |
| `L-03` | Prompt A captured three real channels; prompt C Chrome baseline found an English track hidden and no drawn text (§2.1); acceptance open | Shared-transport physical pass, M3 prompt B and remaining prompt C native rows, M2 client device pass and M4 post-advertising caption pass. #482 already merged. |
| `W-01` | No acceptance run | Progressive-remux browser/device matrix, post-deploy event rate and journal. |
| `W-02` | Readiness repair; no §5.4–5.5 acceptance | Browser, lock-screen, headset and LG/Fire TV input evidence; 5.4/5.5 also await 5.1-5.3 type baseline and Playwright. |
| `A-01` | No acceptance run | Apple TV HDMI mode matrix; iPhone interruption/route matrix. |
| `A-02` | §5.5 branch build passed; tests unrun | Controller device evidence per plan §6 after remaining 5.1-5.6. |
| `A-03` | No acceptance run | Deployed `sort_title` curl; Apple/Android paging/filter traces after 5.2-5.5. |
| `A-04` | Chrome exact-main trace failed; two-cliff timed out before first frame; Apple TV asleep and iPhone locked refused trace launch | D3 shaped-network baseline: Safari/Firefox, Apple TV/iPhone, four Android types, two cliff profiles, six metrics and HDR second pass. |
| `A-05` | Blocked by A-04 D3 | Platform's shaped trace on an unmerged milestone build before Auto controller merges. |
| `D-01` | No acceptance run | Three-TV ADB memory/display baseline and physical HDMI pass. |
| `D-02` | No acceptance run | §5.6 Android lifecycle/player device acceptance after M1-M5/M7-M9. |
| `D-03` | Four attached Android devices have debuggable versionCode 124; acceptance open | Lenovo trace; signed release build/install, device M9/M10. Signing inputs absent; release-role change is on draft PR #23; Xiaomi and Lenovo absent. |
| `P-02` | Preliminary snapshot in §2; acceptance open | Busy-evening sample and one week without EMFILE. |

`P-03` explicitly names no fleet or device evidence. The [workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) remains the status authority; this page is a dated observation and owed-work inventory.
