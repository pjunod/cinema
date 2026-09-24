# Media body buffers M1: before/after measurement

**Status:** recorded · **Measured:** 2026-09-24 · **Plan:**
[MEDIA-BODY-BUFFERS.md](../streaming/MEDIA-BODY-BUFFERS.md) §5.1 (workboard
row S-02) · **Source:** `main` @ `886fc8bd4`

This is the raw record behind the §5.1 result in the plan. The summary and
the verdict are in the plan. This file keeps the method, the conditions, the
harness and every number it produced, so the run can be repeated and checked.

## What was compared, and why here

The §5.1 acceptance compares builds. It asks for the same node, the same flags
and generated fixtures, before and after the change. It does not ask for a
production library. That settled where to run it:

- **nuc4 could not supply the "before" side.** Its `plurxd` container runs
  image revision `99d4abf8c80c2c78cd32c7978186a4c64b136ed0`
  (`org.opencontainers.image.revision`, created 2026-09-24T03:47Z). The M1
  code (`ed98c6abc`, `be3c082d4`, merged through `6683eaa4a` / #454) is an
  ancestor of it, so nuc4 already runs the "after" build. It is also a
  production node with viewers, and nothing in the protocol can control that.
  The only things run against nuc4 were `docker ps`, `docker inspect` and two
  unauthenticated `curl`s to 404 paths, all read-only. No stream was started
  there and no token was created there.
- **The comparison ran on nuc3 instead.** Three release builds of one tree
  (`886fc8bd4`) differ in a single line,
  `MEDIA_BODY_READ_BUFFER` in `crates/plurxd/src/media_sessions.rs`:
  `4 * 1024` (the before side), `128 * 1024` (the Decision 1 fallback) and
  `256 * 1024` (as merged). `MEDIA_BODY_ACK_GRANULARITY` stays 4 KiB in all
  three. At 4 KiB, `ReaderStream::with_capacity(r, 4096)` is exactly
  `ReaderStream::new(r)`, and the pump splits each read into one 4 KiB piece.
  At the measured sites that is the pre-M1 behaviour, without the 96 unrelated
  commits between M1 and this tree. The `offline.rs` and
  `internal_media.rs` readers also follow the constant, and they were already
  256 KiB before M1. The harness exercises neither.
- The build was `cargo build --locked --release -p plurxd` with rustc 1.97.1
  (the release profile: thin LTO, stripped). The sha256 of each binary:
  `90fc8954…d5c` (4k), `63449051…b3` (128k), `2b76f68a…2cd` (256k).

## Conditions

| Field | Value |
|---|---|
| Host | nuc3: Intel Core i5-1240P (12 cores, 16 threads), 31 GiB RAM, kernel 7.0.0-31, ext4 on LVM |
| Host load | Shared build host. Other agents' test loops ran throughout, and the 1-minute load average at the start of each variant was 1.1 to 8.7 (in the raw table). This is **not** the "nothing else running" lab4 condition §5.1 names. Each trial rotates the variant order to spread drift, and there are three trials. |
| Server | Loopback `127.0.0.1:39400`, fresh data dir, one Home library, `PLURX_LOG=warn`, glibc allocator (plurxd sets no `global_allocator`) |
| Client | curl 8.18.0 on the same host, over loopback |
| Fixtures | `scripts/bench fixtures --only 1080p-h264` and `--only 4k-hdr10`. `4k-hdr10.mkv`: HEVC Main10 PQ, 3840x2160, 45.02 s, **93,628,072 bytes, 16.6 Mb/s** (the plan's "~80 Mb/s" was an estimate; the generator's ultrafast testsrc2 yields this). Photo: one 1600x1200 JPEG, **278,702 bytes** (`ffmpeg testsrc2 + noise, -q:v 5`). |
| Storage | All media was in the page cache (warm) for every timed run. So this measures the blocking-pool hop and copy cost, not disk. |
| Isolation | Each group ran in a fresh `plurxd` process against the same data dir, started, health-checked, then left idle 10 s: single+A in one, B in one, HLS in one. This keeps allocator memory retained by one group out of the next group's peak. |

## Method, where it departs from the §5.1 text

- **Peak RSS is `VmHWM`, reset per group.** The harness writes `5` to
  `/proc/<pid>/clear_refs` just before a group, which resets the high-water
  mark to the current RSS, and reads `VmHWM` just after. The plan's 1 Hz
  `VmRSS` sampler also ran, but group A lasts 0.1 to 0.7 s and it recorded no
  sample at all (`A_rss1hz_max_kb 0` in the raw data). A 1 Hz sampler cannot
  measure this group. The table reports both the absolute `VmHWM` and the
  growth over the RSS just before the group.
- **Peak thread count** is `Threads:` from `/proc/<pid>/status` polled every
  50 ms, the same quantity as `ps -o nlwp`.
- **The photo URL in §5.1 did not exist.** `/api/v1/photos/{id}/original` is
  not a route. Photo originals are served by `GET /api/v1/items/{id}/photo`
  (`photos::serve`, which calls `serve_file_range`). The harness uses that,
  and this PR updates §5.1.
- **HLS: one untimed pass first, then the 200 timed GETs.** The session is a
  software x264 tone-map transcode on nuc3 (`encoder: software (x264)`,
  `vod: true`, 23 segments of about 5 MB each at 2 s). If the loop started on
  a cold session, its first pass would time ffmpeg, not the body path. The
  harness fetches every segment once and records that pass separately (about
  53 to 58 s in total for every variant). Then it runs the 200 timed GETs
  exactly as §5.1 writes them.
- **Group B concurrency was measured afterwards.** The three main trials did
  not record how many of the 128 requests were in flight at once, and group B
  RSS turned out to depend on that. Three more group B runs per variant
  (`inflight.sh` below) sampled server-side established connections on the
  port every 50 ms.

## Harness

`bench.sh` is the main run: three trials, variant order `4k 128k 256k`, then
`256k 4k 128k`, then `128k 256k 4k`. `inflight.sh` is the group B rerun that
records concurrency. `hlsdiag.sh` records the per-segment HLS timing and the
syscall counts in the diagnostic section. The token is the throwaway server's
own `/setup` admin token. It was deleted with that server's data dir after the
runs.

## Results

MB/s is decimal (curl `speed_download` / 10^6). RSS is MiB (KiB / 1024).
Every cell lists trial 1, trial 2 and trial 3.

| Figure (§5.1) | 4 KiB (before) | 128 KiB | 256 KiB (merged) |
|---|---|---|---|
| Single viewer, direct play, MB/s (3 runs x 3 trials; median) | 364, 329, 371 · 390, 403, 373 · 355, 459, 395 (**373**) | 2224, 3188, 3904 · 2469, 2735, 4251 · 3341, 4225, 3991 (**3341**) | 2415, 4286, 3766 · 2565, 3172, 2886 · 2991, 4319, 4666 (**3172**) |
| Group A: 8 concurrent direct plays, wall s | 0.70, 0.71, 0.69 | 0.10, 0.11, 0.11 | 0.11, 0.11, 0.10 |
| Group A: peak RSS (`VmHWM`) MiB | 102.8, 103.8, 99.9 | 113.9, 114.2, 112.7 | 113.8, 113.5, 114.5 |
| Group A: growth over pre-group RSS, MiB | **0.96, 1.21, 1.25** | **9.8, 10.9, 9.3** | **15.4, 14.3, 16.1** |
| Group A: peak threads | 46, 47, 47 | 40, 38, 35 | 30, 29, 35 |
| Group B: 20 rounds of 64 `-r 0-1` probes + 64 photo originals, wall s | 3.23, 3.25, 3.02 | 2.20, 2.43, 2.24 | 2.52, 2.48, 2.16 |
| Group B: peak RSS (`VmHWM`) MiB | 136.6, 135.3, 140.6 | 143.1, 142.3, 143.0 | 142.4, 148.2, 148.8 |
| Group B: growth over pre-group RSS, MiB | **34.0, 32.9, 39.0** | **42.1, 40.2, 40.2** | **47.0, 51.9, 53.0** |
| Group B: peak threads | 69, 60, 62 | 43, 39, 38 | 37, 37, 38 |
| Group B: failed requests | 0, 0, 0 | 0, 0, 0 | 0, 0, 0 |
| HLS 200 GETs p50 ms | **24.4, 25.3, 24.0** | 50.0, 49.1, 48.2 | **50.0, 49.2, 48.2** |
| HLS p95 ms | 61.6, 63.7, 64.5 | 57.6, 58.2, 51.1 | 56.6, 55.7, 50.4 |
| HLS p99 ms | **65.2, 72.4, 70.6** | 58.8, 59.7, 51.3 | **58.0, 57.3, 51.1** |
| HLS max ms | 69.3, 75.5, 79.2 | 60.3, 59.9, 51.8 | 59.2, 57.9, 51.2 |
| Idle threads after start | 27, 27, 26 | 26, 27, 26 | 27, 27, 26 |

Group B again, with the peak number of established server-side connections
recorded (three more runs per variant, in the order 4k, 128k, 256k each time):

| Run | 4 KiB: growth MiB / peak connections | 128 KiB | 256 KiB |
|---|---|---|---|
| I1 | 50.5 / 105 | 41.6 / 61 | 53.2 / 104 |
| I2 | 58.9 / 122 | 74.2 / 108 | 69.9 / 103 |
| I3 | 55.1 / 127 | 67.3 / 127 | 89.5 / 127 |

Across all six group B runs, the growth medians are 44.8 MiB (4 KiB),
41.9 MiB (128 KiB) and 53.1 MiB (256 KiB). The ranges are 32.9 to 58.9,
40.2 to 74.2 and 47.0 to 89.5.

### HLS per-segment diagnostic

The HLS p50 doubled while p99 fell, so one session per build was fetched
again segment by segment. Each segment was fetched twice after a warming
pass. The table uses the second fetch, with `/proc/<pid>/io` `syscr`/`syscw`
deltas taken around each request:

| | 4 KiB | 256 KiB |
|---|---|---|
| Time per ~5 MB segment | 15 to 70 ms, 16 of 23 between 21 and 32 ms | bimodal: 10 of 23 at 8 to 14 ms, 13 of 23 at 47 to 55 ms |
| Read syscalls per segment | 615 to 1,418 | 10 to 41 |
| Write syscalls per segment | 1,222 to 2,541 | 742 to 1,619 |
| Time to first byte | 0.5 to 1.0 ms | 0.5 to 1.4 ms |

The read count fell by about 60x, as §1 predicted. The latency change is
not in time to first byte. It is a mode near 50 ms that most 256 KiB fetches
land in. The 4 KiB build lands there too, but only in its tail (its p95 to
max is 61 to 79 ms). A cause that fits the numbers, **not proven here**:
`plurxd` sets no `TCP_NODELAY` anywhere (`grep -rn nodelay crates/plurxd/src`
finds nothing), and a ~40 ms stall on loopback is Linux's minimum delayed-ACK
time meeting Nagle's algorithm on a trailing small write. How often that
happens depends on write timing, which the read size changes. Loopback is
also not a network path. Whether the p50 shift survives on a real client
link is open.

## Against the §5.1 acceptance

§5.1 accepts M1 when "all five are reported and … p99 and both peak-RSS
figures did not get worse". Group B has a veto if "peak RSS there scales at
roughly half a megabyte per concurrent small request".

- **All five reported:** yes, above.
- **HLS p99 not worse:** met. 256 KiB: 51 to 58 ms. 4 KiB: 65 to 72 ms.
  The p50 moved the other way, 24 to 25 ms before and 48 to 50 ms after
  (see the diagnostic).
- **Group A peak RSS not worse:** **not met.** Growth under 8 large viewers
  went from about 1 MiB to 14.3 to 16.1 MiB. That is about 1.8 MiB per
  concurrent body over the 4 KiB build, above the "≈1 MiB per active body"
  budget in §2.1. 128 KiB grew 9.3 to 10.9 MiB (about 1.1 MiB per body).
- **Group B peak RSS not worse:** **not met in the median.** The six-run
  medians are 44.8 MiB (4 KiB) and 53.1 MiB (256 KiB). Group B memory is
  dominated by per-connection cost, which the 4 KiB build shows at 33 to
  59 MiB, and it varies with how many requests are in flight at once.
  **The veto condition is not reached.** The worst run is I3: 89.5 MiB
  against 55.1 MiB, both at 127 concurrent connections. That is about
  0.27 MiB per in-flight request, about half of the veto line. I1, at 104
  against 105 connections, gives about 0.03 MiB. I2 ran at unequal
  concurrency (103 against 122 connections) and gives no per-request figure.
- **Throughput and blocking-pool cost:** single-viewer direct play rose
  about 8.5x (median 373 → 3172 MB/s, page cache warm, loopback). Group A
  wall time fell from 0.70 s to 0.11 s. Peak thread count fell under both
  groups, and read syscalls per HLS segment fell about 60x. 128 KiB and
  256 KiB are indistinguishable on every throughput and latency figure
  within run-to-run spread.

## Raw data

### results.tsv (bench.sh: trial, variant, key, value)

```text
1	4k	load	1.13 5.15 6.48
1	4k	base_rss_kb	103648
1	4k	base_threads	27
1	4k	single_Bps_time	364048089 0.257186
1	4k	single_Bps_time	328999775 0.284584
1	4k	single_Bps_time	371419109 0.252082
1	4k	preA_rss_kb	104332
1	4k	A_wall_s	.703001168
1	4k	A_hwm_kb	105312
1	4k	A_peak_threads	46
1	4k	A_rss1hz_max_kb	0
1	4k	preB_rss_kb	105012
1	4k	B_wall_s	3.226785094
1	4k	B_failures	0
1	4k	B_hwm_kb	139828
1	4k	B_peak_threads	69
1	4k	B_rss1hz_max_kb	138880
1	4k	hls_segments	23
1	4k	hls_first_pass	n 23 sum 53.5122 max 6.037774
1	4k	hls_200	n 200 p50 0.024400 p95 0.061643 p99 0.065183 max 0.069266
1	4k	hls_delete	204
1	128k	load	6.54 5.83 6.56
1	128k	base_rss_kb	104756
1	128k	base_threads	26
1	128k	single_Bps_time	2224473081 0.042090
1	128k	single_Bps_time	3187555646 0.029373
1	128k	single_Bps_time	3904260539 0.023981
1	128k	preA_rss_kb	106576
1	128k	A_wall_s	.103543774
1	128k	A_hwm_kb	116608
1	128k	A_peak_threads	40
1	128k	A_rss1hz_max_kb	0
1	128k	preB_rss_kb	103456
1	128k	B_wall_s	2.199815858
1	128k	B_failures	0
1	128k	B_hwm_kb	146524
1	128k	B_peak_threads	43
1	128k	B_rss1hz_max_kb	145520
1	128k	hls_segments	23
1	128k	hls_first_pass	n 23 sum 55.8996 max 6.024692
1	128k	hls_200	n 200 p50 0.049962 p95 0.057560 p99 0.058793 max 0.060270
1	128k	hls_delete	204
1	256k	load	6.06 5.69 6.41
1	256k	base_rss_kb	98372
1	256k	base_threads	27
1	256k	single_Bps_time	2414588198 0.038776
1	256k	single_Bps_time	4285822210 0.021846
1	256k	single_Bps_time	3766213676 0.024860
1	256k	preA_rss_kb	100704
1	256k	A_wall_s	.108564727
1	256k	A_hwm_kb	116484
1	256k	A_peak_threads	30
1	256k	A_rss1hz_max_kb	0
1	256k	preB_rss_kb	97708
1	256k	B_wall_s	2.524482692
1	256k	B_failures	0
1	256k	B_hwm_kb	145848
1	256k	B_peak_threads	37
1	256k	B_rss1hz_max_kb	142428
1	256k	hls_segments	23
1	256k	hls_first_pass	n 23 sum 53.3952 max 6.058456
1	256k	hls_200	n 200 p50 0.049970 p95 0.056599 p99 0.057990 max 0.059151
1	256k	hls_delete	204
2	256k	load	7.46 6.24 6.51
2	256k	base_rss_kb	97940
2	256k	base_threads	27
2	256k	single_Bps_time	2564660804 0.036507
2	256k	single_Bps_time	3172005014 0.029517
2	256k	single_Bps_time	2885747326 0.032445
2	256k	preA_rss_kb	101556
2	256k	A_wall_s	.113812110
2	256k	A_hwm_kb	116220
2	256k	A_peak_threads	29
2	256k	A_rss1hz_max_kb	0
2	256k	preB_rss_kb	98616
2	256k	B_wall_s	2.481937366
2	256k	B_failures	0
2	256k	B_hwm_kb	151772
2	256k	B_peak_threads	37
2	256k	B_rss1hz_max_kb	148592
2	256k	hls_segments	23
2	256k	hls_first_pass	n 23 sum 52.8249 max 6.052371
2	256k	hls_200	n 200 p50 0.049181 p95 0.055734 p99 0.057286 max 0.057853
2	256k	hls_delete	204
2	4k	load	7.07 6.33 6.50
2	4k	base_rss_kb	104300
2	4k	base_threads	27
2	4k	single_Bps_time	389561841 0.240342
2	4k	single_Bps_time	402928411 0.232369
2	4k	single_Bps_time	372822655 0.251133
2	4k	preA_rss_kb	105036
2	4k	A_wall_s	.708742761
2	4k	A_hwm_kb	106272
2	4k	A_peak_threads	47
2	4k	A_rss1hz_max_kb	0
2	4k	preB_rss_kb	104932
2	4k	B_wall_s	3.245238049
2	4k	B_failures	0
2	4k	B_hwm_kb	138576
2	4k	B_peak_threads	60
2	4k	B_rss1hz_max_kb	137852
2	4k	hls_segments	23
2	4k	hls_first_pass	n 23 sum 53.2434 max 6.048732
2	4k	hls_200	n 200 p50 0.025252 p95 0.063673 p99 0.072440 max 0.075512
2	4k	hls_delete	204
2	128k	load	7.55 6.48 6.52
2	128k	base_rss_kb	104100
2	128k	base_threads	27
2	128k	single_Bps_time	2468573929 0.037928
2	128k	single_Bps_time	2735343480 0.034229
2	128k	single_Bps_time	4250797784 0.022026
2	128k	preA_rss_kb	105740
2	128k	A_wall_s	.108071358
2	128k	A_hwm_kb	116920
2	128k	A_peak_threads	38
2	128k	A_rss1hz_max_kb	0
2	128k	preB_rss_kb	104524
2	128k	B_wall_s	2.430390424
2	128k	B_failures	0
2	128k	B_hwm_kb	145668
2	128k	B_peak_threads	39
2	128k	B_rss1hz_max_kb	144792
2	128k	hls_segments	23
2	128k	hls_first_pass	n 23 sum 52.823 max 5.982816
2	128k	hls_200	n 200 p50 0.049097 p95 0.058204 p99 0.059659 max 0.059898
2	128k	hls_delete	204
3	128k	load	6.40 6.23 6.41
3	128k	base_rss_kb	104160
3	128k	base_threads	26
3	128k	single_Bps_time	3340876788 0.028025
3	128k	single_Bps_time	4225093501 0.022160
3	128k	single_Bps_time	3991476829 0.023457
3	128k	preA_rss_kb	105936
3	128k	A_wall_s	.114119783
3	128k	A_hwm_kb	115440
3	128k	A_peak_threads	35
3	128k	A_rss1hz_max_kb	0
3	128k	preB_rss_kb	105244
3	128k	B_wall_s	2.242013275
3	128k	B_failures	0
3	128k	B_hwm_kb	146456
3	128k	B_peak_threads	38
3	128k	B_rss1hz_max_kb	145796
3	128k	hls_segments	23
3	128k	hls_first_pass	n 23 sum 58.5088 max 5.993612
3	128k	hls_200	n 200 p50 0.048213 p95 0.051079 p99 0.051293 max 0.051789
3	128k	hls_delete	204
3	256k	load	7.89 6.64 6.51
3	256k	base_rss_kb	98104
3	256k	base_threads	26
3	256k	single_Bps_time	2991216638 0.031301
3	256k	single_Bps_time	4319036442 0.021678
3	256k	single_Bps_time	4665773259 0.020067
3	256k	preA_rss_kb	100688
3	256k	A_wall_s	.103635683
3	256k	A_hwm_kb	117224
3	256k	A_peak_threads	35
3	256k	A_rss1hz_max_kb	0
3	256k	preB_rss_kb	98080
3	256k	B_wall_s	2.156183796
3	256k	B_failures	0
3	256k	B_hwm_kb	152332
3	256k	B_peak_threads	38
3	256k	B_rss1hz_max_kb	152320
3	256k	hls_segments	23
3	256k	hls_first_pass	n 23 sum 56.8853 max 6.181207
3	256k	hls_200	n 200 p50 0.048223 p95 0.050441 p99 0.051129 max 0.051233
3	256k	hls_delete	204
3	4k	load	8.65 7.05 6.66
3	4k	base_rss_kb	100268
3	4k	base_threads	26
3	4k	single_Bps_time	355265428 0.263544
3	4k	single_Bps_time	459037643 0.203966
3	4k	single_Bps_time	394921849 0.237080
3	4k	preA_rss_kb	101044
3	4k	A_wall_s	.687959045
3	4k	A_hwm_kb	102328
3	4k	A_peak_threads	47
3	4k	A_rss1hz_max_kb	0
3	4k	preB_rss_kb	104032
3	4k	B_wall_s	3.020669104
3	4k	B_failures	0
3	4k	B_hwm_kb	144008
3	4k	B_peak_threads	62
3	4k	B_rss1hz_max_kb	144008
3	4k	hls_segments	23
3	4k	hls_first_pass	n 23 sum 55.6356 max 6.986701
3	4k	hls_200	n 200 p50 0.023981 p95 0.064489 p99 0.070565 max 0.079193
3	4k	hls_delete	204
```

### inflight.sh (three runs: I1, then I2 and I3)

```text
4k pre_rss_kb=103532 hwm_kb=155288 peak_established=105
128k pre_rss_kb=104100 hwm_kb=146728 peak_established=61
256k pre_rss_kb=97644 hwm_kb=152092 peak_established=104
4k pre_rss_kb=104964 hwm_kb=165276 peak_established=122
128k pre_rss_kb=104660 hwm_kb=180628 peak_established=108
256k pre_rss_kb=98804 hwm_kb=170336 peak_established=103
4k pre_rss_kb=104220 hwm_kb=160624 peak_established=127
128k pre_rss_kb=103776 hwm_kb=172700 peak_established=127
256k pre_rss_kb=99512 hwm_kb=191152 peak_established=127
```

### hlsdiag.sh, second fetch of each segment (variant, rep, segment, bytes, ttfb s, total s, syscall deltas)

```text
4k 2 seg00000.m4s 5806277 0.000548 0.030227 syscw=1647 syscr=1418
4k 2 seg00001.m4s 4831019 0.000508 0.022595 syscw=1886 syscr=1180
4k 2 seg00002.m4s 4965347 0.000574 0.021747 syscw=1468 syscr=1213
4k 2 seg00003.m4s 5028892 0.000655 0.029808 syscw=2375 syscr=1228
4k 2 seg00004.m4s 4857036 0.000561 0.024990 syscw=1591 syscr=1186
4k 2 seg00005.m4s 5046444 0.000554 0.023709 syscw=2470 syscr=1233
4k 2 seg00006.m4s 4907958 0.000543 0.026042 syscw=1768 syscr=1199
4k 2 seg00007.m4s 5191384 0.000544 0.061186 syscw=2541 syscr=1268
4k 2 seg00008.m4s 5067940 0.001010 0.040007 syscw=1764 syscr=1238
4k 2 seg00009.m4s 5150902 0.000536 0.022783 syscw=2521 syscr=1258
4k 2 seg00010.m4s 4911610 0.000499 0.022288 syscw=1873 syscr=1200
4k 2 seg00011.m4s 5104611 0.000562 0.028182 syscw=2253 syscr=1247
4k 2 seg00012.m4s 4957872 0.000487 0.066426 syscw=1927 syscr=1211
4k 2 seg00013.m4s 5059257 0.000795 0.031243 syscw=2171 syscr=1236
4k 2 seg00014.m4s 5094729 0.000868 0.070367 syscw=1539 syscr=1244
4k 2 seg00015.m4s 5127512 0.000901 0.035531 syscw=1878 syscr=1252
4k 2 seg00016.m4s 4954075 0.000492 0.026953 syscw=2358 syscr=1210
4k 2 seg00017.m4s 5048451 0.000506 0.066325 syscw=1804 syscr=1233
4k 2 seg00018.m4s 4981125 0.000832 0.024366 syscw=1222 syscr=1217
4k 2 seg00019.m4s 5006298 0.000518 0.028752 syscw=2450 syscr=1223
4k 2 seg00020.m4s 5059006 0.000545 0.022069 syscw=2476 syscr=1236
4k 2 seg00021.m4s 5087073 0.000595 0.024029 syscw=2207 syscr=1242
4k 2 seg00022.m4s 2517470 0.000545 0.015034 syscw=1236 syscr=615
256k 2 seg00000.m4s 5803252 0.000962 0.054101 syscw=1604 syscr=27
256k 2 seg00001.m4s 4834552 0.000869 0.052540 syscw=1493 syscr=41
256k 2 seg00002.m4s 4961189 0.000842 0.014149 syscw=1519 syscr=20
256k 2 seg00003.m4s 5030950 0.000710 0.008062 syscw=1327 syscr=21
256k 2 seg00004.m4s 4857755 0.000521 0.048885 syscw=1462 syscr=20
256k 2 seg00005.m4s 5045558 0.000730 0.008146 syscw=1513 syscr=20
256k 2 seg00006.m4s 4903323 0.000651 0.049061 syscw=1525 syscr=20
256k 2 seg00007.m4s 5195770 0.001395 0.013251 syscw=1602 syscr=23
256k 2 seg00008.m4s 5070197 0.000816 0.052289 syscw=1498 syscr=20
256k 2 seg00009.m4s 5141155 0.001061 0.013317 syscw=1591 syscr=20
256k 2 seg00010.m4s 4913739 0.000818 0.051606 syscw=1527 syscr=19
256k 2 seg00011.m4s 5113293 0.000634 0.012448 syscw=1588 syscr=20
256k 2 seg00012.m4s 4946336 0.000785 0.011389 syscw=1601 syscr=19
256k 2 seg00013.m4s 5056642 0.000772 0.014359 syscw=1551 syscr=20
256k 2 seg00014.m4s 5086433 0.000685 0.050927 syscw=1559 syscr=20
256k 2 seg00015.m4s 5133254 0.001105 0.053405 syscw=1527 syscr=20
256k 2 seg00016.m4s 4976864 0.000620 0.055027 syscw=1619 syscr=19
256k 2 seg00017.m4s 5051967 0.001083 0.052640 syscw=1269 syscr=20
256k 2 seg00018.m4s 4977743 0.001034 0.011896 syscw=1525 syscr=19
256k 2 seg00019.m4s 5010155 0.000675 0.011197 syscw=1252 syscr=20
256k 2 seg00020.m4s 5063200 0.000781 0.050524 syscw=1344 syscr=20
256k 2 seg00021.m4s 5084706 0.001082 0.052423 syscw=1572 syscr=20
256k 2 seg00022.m4s 2528659 0.000633 0.046923 syscw=742 syscr=10
```

### build.sh

```bash
#!/bin/sh
set -e
export PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$HOME/work/s02-target CARGO_INCREMENTAL=0
cd ~/work/hc11
F=crates/plurxd/src/media_sessions.rs
git rev-parse HEAD
for v in 256 4 128; do
  git checkout -q -- $F
  sed -i "s/^pub(crate) const MEDIA_BODY_READ_BUFFER: usize = 256 \* 1024;/pub(crate) const MEDIA_BODY_READ_BUFFER: usize = $v * 1024;/" $F
  grep -n "const MEDIA_BODY_READ_BUFFER" $F
  git diff --stat
  cargo build --locked --release -p plurxd
  cp $CARGO_TARGET_DIR/release/plurxd ~/work/s02/plurxd-${v}k
  echo "BUILT ${v}k $(sha256sum ~/work/s02/plurxd-${v}k)"
done
git checkout -q -- $F
git status --short
```

### bench.sh

```bash
#!/bin/bash
# S-02 M1 A/B: same main tree (886fc8bd4), MEDIA_BODY_READ_BUFFER = 4/128/256 KiB.
set -u
cd ~/work/s02
TOKEN=$(cat token); BASE=http://127.0.0.1:39400
FILE=2            # 4k-hdr10.mkv, 93,628,072 bytes
PHOTO=3555341616529755897   # s02-photo.jpg, 278,702 bytes
OUT=${OUT:-results.tsv}

start() {  # $1 variant
  PLURX_LOG=warn setsid ./plurxd-$1 --config cfg.toml run > srv-$1.log 2>&1 < /dev/null &
  SRV=$!
  for i in $(seq 100); do curl -sf -o /dev/null $BASE/healthz && break; sleep 0.2; done
  sleep 10
  PID=$SRV; [ "$(cat /proc/$PID/comm)" = "plurxd-$1" ] || { echo BADPID; exit 9; }
}
stop() { kill $PID; wait $PID 2>/dev/null; sleep 2; }
status() { awk -v k=$1 '$1==k":"{print $2}' /proc/$PID/status; }
peak_threads_start() {
  ( m=0; while kill -0 $PID 2>/dev/null; do t=$(awk '/^Threads:/{print $2}' /proc/$PID/status 2>/dev/null); [ -n "$t" ] && [ "$t" -gt "$m" ] && { m=$t; echo $m > peak_threads.$1; }; sleep 0.05; done ) &
  TS=$!
}
sampler_start() { ( while sleep 1; do awk '/VmRSS/{print systime(), $2}' /proc/$PID/status; done ) > rss-$1.log & SAMP=$!; }
rec() { printf "%s\t%s\t%s\t%s\n" "$TRIAL" "$V" "$1" "$2" | tee -a $OUT; }

for TRIAL in 1 2 3; do
  case $TRIAL in 1) order="4k 128k 256k";; 2) order="256k 4k 128k";; 3) order="128k 256k 4k";; esac
  for V in $order; do
    rec load "$(cut -d" " -f1-3 /proc/loadavg)"
    # --- single viewer + group A (fresh process)
    start $V
    rec base_rss_kb "$(status VmRSS)"; rec base_threads "$(status Threads)"
    for i in 1 2 3; do
      rec single_Bps_time "$(curl -s -o /dev/null -w "%{speed_download} %{time_total}" -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/files/$FILE/direct")"
    done
    rec preA_rss_kb "$(status VmRSS)"
    echo 5 > /proc/$PID/clear_refs
    echo 0 > peak_threads.A; peak_threads_start A; sampler_start A-$TRIAL-$V
    t0=$(date +%s.%N); pids=()
    for v in $(seq 8); do curl -s -o /dev/null -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/files/$FILE/direct" & pids+=("$!"); done
    for p in "${pids[@]}"; do wait "$p"; done
    t1=$(date +%s.%N)
    kill $SAMP; kill $TS; wait $SAMP $TS 2>/dev/null
    rec A_wall_s "$(echo "$t1 - $t0" | bc)"
    rec A_hwm_kb "$(status VmHWM)"; rec A_peak_threads "$(cat peak_threads.A)"
    rec A_rss1hz_max_kb "$(awk 'm<$2{m=$2}END{print m+0}' rss-A-$TRIAL-$V.log)"
    stop
    # --- group B (fresh process)
    start $V
    rec preB_rss_kb "$(status VmRSS)"
    echo 5 > /proc/$PID/clear_refs
    echo 0 > peak_threads.B; peak_threads_start B; sampler_start B-$TRIAL-$V
    t0=$(date +%s.%N); fails=0
    for round in $(seq 20); do
      pids=()
      for v in $(seq 64); do
        curl -sf -o /dev/null -r 0-1 -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/files/$FILE/direct" & pids+=("$!")
        curl -sf -o /dev/null -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/items/$PHOTO/photo" & pids+=("$!")
      done
      for p in "${pids[@]}"; do wait "$p" || fails=$((fails+1)); done
    done
    t1=$(date +%s.%N)
    kill $SAMP; kill $TS; wait $SAMP $TS 2>/dev/null
    rec B_wall_s "$(echo "$t1 - $t0" | bc)"; rec B_failures "$fails"
    rec B_hwm_kb "$(status VmHWM)"; rec B_peak_threads "$(cat peak_threads.B)"
    rec B_rss1hz_max_kb "$(awk 'm<$2{m=$2}END{print m+0}' rss-B-$TRIAL-$V.log)"
    stop
    # --- HLS (fresh process)
    start $V
    SESSION=$(curl -fsS -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
      -d "{\"playback_id\":\"s02-buffer-bench\",\"request_id\":\"s02-$TRIAL-$V-$(date +%s)\",\"height\":2160,\"start\":0}" \
      "$BASE/api/v1/files/$FILE/hls/sessions" | python3 -c "import json,sys; print(json.load(sys.stdin)[\"session_id\"])")
    mapfile -t segments < <(curl -fsS "$BASE/api/v1/hls/$SESSION/index.m3u8" | awk '/^[^#].*\.(ts|m4s)$/{print}')
    rec hls_segments "${#segments[@]}"
    # untimed-for-acceptance first pass: waits for production; reported separately
    warm=$(for s in "${segments[@]}"; do curl -fsS -o /dev/null -w "%{time_total}\n" "$BASE/api/v1/hls/$SESSION/$s"; done | sort -n | awk '{a[NR]=$1;s+=$1} END{print "n",NR,"sum",s,"max",a[NR]}')
    rec hls_first_pass "$warm"
    rec hls_200 "$(for n in $(seq 0 199); do s="${segments[$((n % ${#segments[@]}))]}"; curl -fsS -o /dev/null -w "%{time_total}\n" "$BASE/api/v1/hls/$SESSION/$s"; done | sort -n | awk '{a[NR]=$1} END{print "n",NR,"p50",a[int(NR*.5)],"p95",a[int(NR*.95)],"p99",a[int(NR*.99)],"max",a[NR]}')"
    rec hls_delete "$(curl -sS -o /dev/null -w "%{http_code}" -X DELETE "$BASE/api/v1/hls/$SESSION")"
    stop
  done
done
echo DONE
```

### inflight.sh

```bash
#!/bin/bash
# Group B again, sampling established server-side connections on :39400 at ~20 Hz
cd ~/work/s02; TOKEN=$(cat token); BASE=http://127.0.0.1:39400; FILE=2; PHOTO=3555341616529755897
for V in 4k 128k 256k; do
  PLURX_LOG=warn setsid ./plurxd-$V --config cfg.toml run > srv-inflight-$V.log 2>&1 < /dev/null & PID=$!
  for i in $(seq 100); do curl -sf -o /dev/null $BASE/healthz && break; sleep 0.2; done; sleep 10
  pre=$(awk "/^VmRSS/{print \$2}" /proc/$PID/status); echo 5 > /proc/$PID/clear_refs
  ( m=0; while :; do n=$(ss -Htn state established "( sport = :39400 )" | wc -l); [ $n -gt $m ] && { m=$n; echo $m > inflight.max; }; sleep 0.05; done ) & S=$!
  echo 0 > inflight.max
  for round in $(seq 20); do pids=(); for v in $(seq 64); do
    curl -sf -o /dev/null -r 0-1 -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/files/$FILE/direct" & pids+=("$!")
    curl -sf -o /dev/null -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/items/$PHOTO/photo" & pids+=("$!")
  done; for p in "${pids[@]}"; do wait $p; done; done
  kill $S; wait $S 2>/dev/null
  echo "$V pre_rss_kb=$pre hwm_kb=$(awk "/^VmHWM/{print \$2}" /proc/$PID/status) peak_established=$(cat inflight.max)"
  kill $PID; wait $PID 2>/dev/null; sleep 2
done
echo DONE
```

### hlsdiag.sh

```bash
#!/bin/bash
cd ~/work/s02; TOKEN=$(cat token); BASE=http://127.0.0.1:39400
for V in 4k 256k; do
  PLURX_LOG=warn setsid ./plurxd-$V --config cfg.toml run > srv-diag-$V.log 2>&1 < /dev/null & PID=$!
  for i in $(seq 100); do curl -sf -o /dev/null $BASE/healthz && break; sleep 0.2; done; sleep 10
  S=$(curl -fsS -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "{\"playback_id\":\"s02-diag\",\"request_id\":\"s02-diag-$V-$(date +%s)\",\"height\":2160,\"start\":0}" "$BASE/api/v1/files/2/hls/sessions" | python3 -c "import json,sys; print(json.load(sys.stdin)[\"session_id\"])")
  mapfile -t segs < <(curl -fsS "$BASE/api/v1/hls/$S/index.m3u8" | awk "/^[^#].*\\.(ts|m4s)\$/{print}")
  for s in "${segs[@]}"; do curl -fsS -o /dev/null "$BASE/api/v1/hls/$S/$s"; done
  for rep in 1 2; do for s in "${segs[@]}"; do
    w0=$(awk "/^syscw/{print \$2}" /proc/$PID/io); r0=$(awk "/^syscr/{print \$2}" /proc/$PID/io)
    t=$(curl -fsS -o /dev/null -w "%{size_download} %{time_starttransfer} %{time_total}" "$BASE/api/v1/hls/$S/$s")
    w1=$(awk "/^syscw/{print \$2}" /proc/$PID/io); r1=$(awk "/^syscr/{print \$2}" /proc/$PID/io)
    echo "$V $rep $s $t syscw=$((w1-w0)) syscr=$((r1-r0))"
  done; done
  curl -sS -o /dev/null -X DELETE "$BASE/api/v1/hls/$S"; kill $PID; wait $PID 2>/dev/null; sleep 2
done
echo DONE
```
