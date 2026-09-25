# Architecture review fleet and client baselines — read-only checks on deployed main

**Status:** preliminary evidence · **Build:** `f600d28230222005441cfc62301c306785c852ce` on `nynuc`, `m6`, `nuc4`, `nuc3` · **Observed:** 2026-09-25 02:24–02:27 UTC

Companion to the [fleet deployment receipt](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md) and [workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md). This appendix answers what the deployed fleet exposes for L-01, L-02, C-03, S-04, S-05 and K-06 without changing settings, restarting services, starting playback or injecting a failure. It does not qualify those plans.

## L-01 — the cached guide spans 25 hours

On `nynuc`, the HDHomeRun owner's in-container guide file was
`/var/lib/plurx/cache/runtime/live-tv/guide.json`: **1,190,367 bytes**,
modified at `02:22:54 UTC`. The guide object reported a window from Unix
`1790299370` to `1790389370` (**90,000 seconds, 25 hours**), 55 channels,
and `plurx_live_tv_guide_programmes 3413`; its refresh age was 194 seconds
at the metric scrape. An anonymous
`GET /api/v1/live-tv/guide?hours=336` returned **401, 35 bytes**, as did
`GET /api/v1/settings`. These are authentication response sizes, not the
public clipped guide size. The 336-hour acceptance precondition is not met
by this 25-hour cache, and no valid bearer was available to compare the
public response with the owner-side view or inspect a matching DVR rule.

No sink-interruption test was run. Its required NAS export pause, concurrent
recordings and temporary DVR root change would alter the active fleet.
The idle owner reported `plurx_live_tv_sessions{state="active"} 0`,
`plurx_live_tv_transports 0` and `plurx_live_tv_relay_bytes_total 0` at
`02:27:12 UTC`; those counters cannot establish an active packet rate.

## L-02 — idle counters do not exercise the three fleet prompts

At `02:27:12–14 UTC`, every node reported zero active Live TV sessions and
transports. `plurx_live_tv_settings_reads_total{site="observer",outcome="failed"}`
and `plurx_live_tv_fence_observations_total{state="grace"}` were zero on all
four. `m6` reported owner-peer cache `hit=246`, `resolved=25`,
`invalidated=0`; the other three nodes reported zero for those series.
The source build did not expose `plurx_live_tv_start_plan_total` or
`plurx_live_tv_orphans` series.

The [L-02 §6.3](../features/LIVE-TV-SESSION-FENCE-PEER-TRANSPORT-AND-START.md#63-what-only-the-fleet-can-prove--gpt-prompts)
checks require a leader restart with two sessions, cold/warm ATSC 1.0 and
3.0 starts, and an intentionally undeletable scratch directory. Those
conditions were not created in this read-only pass. The workboard still
lists the plan as unclaimed; these idle counters cannot prove its M1–M4
behavior or the 10-second fence grace.

## C-03 — artwork metrics exist, but no cold/warm byte comparison ran

At approximately `02:25 UTC`, user-route artwork hit counters were
`nynuc=3`, `m6=4`, `nuc4=0`, `nuc3=0`. Every node reported zero
`plurx_artwork_derivatives_total` outcomes for all three width buckets
(`w300`, `w500`, `w780`). An unauthenticated settings request returned 401,
and no Plurx bearer was available for the twenty-URL lab load or an
authenticated Chrome Home load. No browser cache was cleared. Therefore
there is no cold/warm transferred-byte result, p50/p99, 503 count, CPU delta
or device browsing result for [C-03 §6.3–6.4](../server/IMAGE-SERVING-AND-DERIVATIVES.md#63-what-to-measure-f-core-7).

## S-04 and S-05 — non-burn attestation only

The `plurx_engine_attestation_seconds` media-stat readings were:

| Node | Media stat count | Media stat sum | Font spawn count | Font stat count |
|---|---:|---:|---:|---:|
| `nynuc` | 630 | 0.227335 s | 0 | 0 |
| `m6` | 391 | 0.300420 s | 0 | 0 |
| `nuc4` | 532 | 0.434052 s | 0 | 0 |
| `nuc3` | 0 | 0 s | 0 | 0 |

The histogram's first bucket is 100 ms, so these warm aggregate sums and
counts do not resolve individual stat latency. No text-burn session was
active or started for [S-04's media1 prompt](../streaming/FONT-ATTESTATION-AND-BLOCKING-IO.md#6-verification-and-rollout).
The `nynuc` container had no `pgrep` binary, and its last-hour Docker log
contained zero `remux ffmpeg:` lines. Without an encoded text-burn or
progressive-remux producer, that journal absence and the idle process check
do not verify [S-05 §5.3](../streaming/FFMPEG-SPAWN-UNIFICATION.md#53-m3--fleet-check)
environment or first/second time to first segment.

## K-06 — NTP offsets are not peer-clock intervals

All four hosts ran `systemd-timesyncd` and reported `NTPSynchronized=yes`.
The read-only `timedatectl timesync-status` snapshot was:

| Node | NTP offset | NTP delay | Jitter | Packet count |
|---|---:|---:|---:|---:|
| `nynuc` | +401 µs | 532 µs | 722 µs | 723 |
| `m6` | +1.208 ms | 147 µs | 796 µs | 275 |
| `nuc4` | −189 µs | 452 µs | 863 µs | 269 |
| `nuc3` | −1.189 ms | 584 µs | 2.309 ms | 270 |

At `02:24:37–39 UTC`, `/metrics` on all four had **no**
`plurx_cluster_clock_*` series and `chronyc` was absent on each host.
`systemd-timesyncd` offsets are relative to each node's selected NTP source;
they are neither authenticated peer offsets nor the interval uncertainty
required by [K-06 M4](../cluster/CLOCK-SKEW-GUARD-DESIGN.md#6-verification-and-rollout).
The one-hour idle distribution and loaded-path repeat remain unmeasured.

## Reproduce the bounded reads

The SSH key path is an operator input and is never copied to a node. These
commands show the shape of the probes; they do not print credentials or
programme titles.

```bash
ssh -i ~/code/plurx-agent/.ssh-deploy-key nynuc \
  'docker exec plurxd stat -c "%s %y" /var/lib/plurx/cache/runtime/live-tv/guide.json'
ssh -i ~/code/plurx-agent/.ssh-deploy-key nynuc \
  'curl -sS -o /dev/null -w "%{http_code} %{size_download}\n" "http://127.0.0.1:32400/api/v1/live-tv/guide?hours=336"'
for host in nynuc m6 nuc4 nuc3; do
  ssh -i ~/code/plurx-agent/.ssh-deploy-key "$host" \
    'curl -fsS http://127.0.0.1:32400/metrics | grep -E "^plurx_(cluster_clock_|artwork_|engine_attestation_|live_tv_)"; timedatectl timesync-status --no-pager'
done
```

**How to read it:** HTTP 401 is an authentication boundary, not an empty
guide. Zero counters on an idle system show instrument availability and a
starting point; only a measured before/after operation can establish the
plan's behavior. The four NTP offsets are corroborating host health, not
clearance under K-06's 2,000 ms peer upper-bound rule.
