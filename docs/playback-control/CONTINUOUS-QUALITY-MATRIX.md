# Continuous-quality matrix — what the retained receipts establish

**Status:** incomplete · **Inventoried:** 2026-10-08 ·
**Candidate baseline:** `fe0ed0096` · **Measured server component:** `67520f7a8`

Companion to [CONTINUOUS-QUALITY-BUILD.md](CONTINUOUS-QUALITY-BUILD.md#83-acceptance-is-a-per-switch-result-not-an-average).
This inventory separates current component evidence from older or missing
source/lifecycle/pressure rows. It does not replace the build ledger or
qualify a newer integrated candidate. The parent session owns integration
and the final platform-wide acceptance decision.

## 1. Read source and scope before a pass

The inventory inspected 192 top-level playback-lab JSON reports and the named
sidecars in the agent-owned ignored reports directory. "Absent" means no
qualifying receipt was located in that set or its documented ledger, not that
a receipt cannot exist elsewhere. Report `server.build` is authoritative;
a source string embedded in a filename is insufficient. Separate native
device evidence stays with the platform owners.

A current scoped pass proves only the observation described in its row.
A safe retained-current result is not a successful quality transition.
Twenty consecutive completed transitions, including five actual Auto changes
where supported, are still required for each platform and family. A failure
ends its series; no diagnostic helper aggregates retries into a pass.

## 2. Source, lifecycle and pressure rows

| Required row | Evidence classification | Observed | Still required |
|---|---|---|---|
| Desktop Firefox / normal / 1x | current scoped pass | Source `67520f7a8`: fifteen manual and five actual Auto transitions; one session/player; zero reported hitches/stalls. | Callback/transport only; normal/fullscreen and mute state are not separately attested. Full pixel/audio acceptance remains open. |
| Headed Firefox / capture campaign | current failed | Fifteen manual; p95 video gap 169.62 ms > 100 ms; Auto absent. Capture incomplete: 221 holes, 76 skipped counters; upper hold 299.988 ms. | Keep failed. Browser optical session owns diagnosis; this inventory does not erase it. |
| Desktop Chrome | older evidence | Earlier full campaigns described in build ledger; no 67520f7a8 Chrome report among 192 parsed playback reports. | Current-source full campaign and independent display/audio evidence. |
| Fullscreen | absent explicit row | No explicit fullscreen-state continuous-quality receipt located. Headed viewport size alone does not prove fullscreen. | Normal→fullscreen→normal with actual fullscreen element and same attachment; switch in each state. |
| Muted / unmuted | absent explicit row | Current continuous fixture has AAC, but presence of audio is not mute-state or captured-audio evidence. | Record actual muted/volume state, switching in both states and independent audio capture for unmuted. |
| Subtitles | absent continuous-family row | Current MPEG4/AAC source metadata says subtitles=[]. Existing subtitle-toggle only checks selected/showing text track. | Caption-bearing family; visible cue/time across switches; text-track showing alone cannot prove pixels. |
| 0.5x rate | current mixed/failed | Current no-D3 short pixel capture passes 95.876 ms upper hold; same report fails one callback hitch. D3-enabled current capture fails 162.572 ms. No quality request in these windows. | Keep every result scoped; current rate-plus-switch and audio evidence absent. |
| 2x rate | older steady pass | Source eab5fceb8: measured 2.00546× over 12 s; zero reported stall/hitch/reopen. | No quality transition in rate window; current-source rate-plus-switch evidence absent. |
| Long prebuffer | older/mixed diagnostic | Older eab5fceb8/0e778892d source prebuffer diagnostics and full campaigns have buffering observations; no explicit current long-prebuffer acceptance row. | Record retained frontier/runway, delayed target boundary and bounded physical work without treating preparation expiry as presentation expiry. |
| Rapid choices | current scoped pass | 720→480→720 final 720p presents in 4,056 ms; same session/player/family; zero reported hitches/stalls. | Focused lifecycle evidence only; no pixel/audio acceptance. |
| Pause / resume | older scoped pass | Source 25ecd16f4: eight-second pause; Parsed readiness; same session/player; observed 0.999×; zero reported hitches/stalls. | Current-source repeat and pending-switch pause/resume. Do not relabel as app background. |
| Seek collision | current scoped pass | Final 720p presents in 2,756 ms; same session/player/family; zero reported hitches/stalls. | Separate receipt because harness deduplicates identical fixture/quality/operation names; no physical acceptance. |
| Background / foreground | absent actual lifecycle row | scripts/playback-lab suspend-resume calls media.pause()/play() and labels surrogate=paused-media-element. | Actual app/tab visibility lifecycle and restored control ownership required; paused media is not background. |
| Explicit End | current process-scoped pass | Current full headless campaign producer_end samples have zero child FFmpeg processes at 0/1/3/5 s. | Does not prove physical queues, all private reader leases or pending callbacks have retired. |
| Natural end / tail switch | older scoped pass | Source 97967c6ce: start 1785 s; target 720p at 1792 s; natural EOF 1800 s; zero End producer counts. | Current tail. Target 1792..1794 artifact was not copied, so independent target join binding absent. |
| Real high-bitrate title | absent | Inspected current fixture is synthetic MPEG4/AAC 1080p, 4.2 Mb/s. | One authorized real title with source identity, measured bitrate/decode load, compatible family and exact receipt. |
| Low-end device | absent qualifying physical row | No low-end-device acceptance established by inspected current desktop reports. | Physical low-end runtime; software emulator is a separate scope. Coordinate with platform owners. |
| VFR / nonzero source origin | absent | Current source fixture is 24 fps CFR. | Measured source shape and actual family eligibility; truthful unsupported result if normalization cannot satisfy invariants. |
| Source-copy / HDR / HEVC | absent current qualifying row | These are distinct media compatibility cases in §5.1; CFR AVC passes do not transfer. | Separate exact-family eligibility and switch/boundary results. Do not force unsafe enrollment. |
| Decoder-limited case | older intervention only | 087e27680 explicitly co-locates source decoder and target on one CPU; incumbent retained then Retry works. | Separate from healthy switch acceptance. Current device/decode-limited case absent; preserve foreground thread policy. |
| Network cliff / recovery | current scoped pass | Full current Firefox campaign has five actual Auto transitions with catalog-derived shaping. | Callback/transport evidence only; physical output and other source/device cases still open. |
| Occupied foreground pool / Retry | older intervention pass | 75c9c7ba3: two viewers; occupied 9 credits; limit lowered 32→9; target refused safely; restoring limit permits shipped Retry. | Current-source normal-load qualification absent. Explicit budget intervention is not physical pressure. |
| Target CPU / source-read contention | older scoped pass | 087e27680: competing decode and target active on one CPU; observed read_chars; both read_bytes deltas zero. | Current-source contention absent; no disk-pressure or shared-reader-fairness claim. |
| Shared-reader fairness | absent | Existing two-viewer pressure pass does not measure ownership/fairness of a shared source reader. | Concurrent reader identity, progress per consumer, cancellation of one without starvation of the other. |
| Physical disk pressure | absent | Retained CPU/read probe recorded zero physical read_bytes. | Bounded owned-file I/O with actual physical read/write evidence and incumbent continuity; no user caches or global settings. |
| Safari / iOS / tvOS / Android matrix | separately owned/incomplete | Platform owners retain native receipts and failures; desktop evidence cannot qualify these rows. | Obtain their source/device-specific runtime evidence through parent; full acceptance remains open. |

## 3. Exact receipts behind the matrix

All names below are relative to `target/playback-lab/reports/`. Preserve the
original files, including failures. The [companion JSON inventory](../../tests/playback/continuous-quality/source-matrix-inventory.json)
records SHA-256 for these inspected files; it is an inventory rather than a
new runtime receipt.

| Row | Receipt | Reported server source |
|---|---|---|
| Desktop Firefox / normal / 1x | `firefox-67520f7a8-worker-full20-quota-retry3.json` | `67520f7a8` |
| Headed Firefox / capture campaign | `firefox-67520f7a8-full20-optical2.json` | `67520f7a8` |
| Headed Firefox / capture campaign | `firefox-67520f7a8-full20-optical2-optical.json` | Sidecar; bind through parent report |
| 0.5x rate | `firefox-67520f7a8-half-speed-optical-no-d3-1.json` | `67520f7a8` |
| 0.5x rate | `firefox-67520f7a8-half-speed-optical-no-d3-1-optical.json` | Sidecar; bind through parent report |
| 0.5x rate | `firefox-67520f7a8-half-speed-optical-d3-enabled2.json` | `67520f7a8` |
| 2x rate | `firefox-eab5fceb8-rate-windows12-first1.json` | `eab5fceb8` |
| 2x rate | `firefox-eab5fceb8-rate-windows12-first1-actions.json` | Sidecar; bind through parent report |
| Rapid choices | `firefox-67520f7a8-selection-lifecycle-current1.json` | `67520f7a8` |
| Pause / resume | `firefox-25ecd16f4-pause-fixed1.json` | `25ecd16f4` |
| Pause / resume | `firefox-25ecd16f4-pause-fixed1-phases.json` | Sidecar; bind through parent report |
| Seek collision | `firefox-67520f7a8-selection-seekcollision-current1.json` | `67520f7a8` |
| Explicit End | `firefox-67520f7a8-worker-full20-quota-retry3.json` | `67520f7a8` |
| Natural end / tail switch | `continuous-chrome-97967c6ce-tail-one-switch.json` | `97967c6ce` |
| Natural end / tail switch | `continuous-chrome-97967c6ce-tail-copied-media-probe.json` | Sidecar; bind through parent report |
| Decoder-limited case | `continuous-chrome-087e27680-targeted-target-source-load1.json` | `087e27680` |
| Network cliff / recovery | `firefox-67520f7a8-worker-full20-quota-retry3.json` | `67520f7a8` |
| Occupied foreground pool / Retry | `continuous-chrome-75c9c7ba3-targeted-foreground-pressure1.json` | `75c9c7ba3` |
| Target CPU / source-read contention | `continuous-chrome-087e27680-targeted-target-source-load1.json` | `087e27680` |

## 4. Focused lifecycle helper — record state without claiming capture

[`scripts/continuous-quality-lifecycle`](../../scripts/continuous-quality-lifecycle)
uses the existing isolated playback lab. One invocation runs exactly one
named diagnostic on one fresh daemon/profile. It checks the daemon build,
retains phase snapshots, checks session/player/family/element/HLS/MediaSource
identity, measures the requested clock rate, preserves hitch/stall counters,
and samples owned FFmpeg retirement at 0, 1, 3 and 5 seconds after End.
The original continuous-suite event/gap limits stay in place. Existing output
paths are refused; a failed observation is retained without an internal retry.

```bash
scripts/continuous-quality-lifecycle \
  --server /owned/source/target/debug/plurxd \
  --source-ref 'verified-server-build' \
  --case pause --browser firefox \
  --fixtures /owned/fixtures \
  --json /owned/reports/pause-build-1.json
```

Choose `pause`, `rate-2`, or `mute-cycle`. Pause uses the shipped Pause/Play
control and holds for eight seconds; rate and mute cases manipulate the
existing media element in the isolated profile and observe eight-second
windows. The initial two-second baseline is reported separately from these
windows. The pause interval remains visible in phase snapshots even though
the moving-frame window starts after resume.

**How to read it:** `passed` means the named state/clock/identity diagnostic
and the owned direct-child process census passed. These cases make no
quality request. They cannot close rate-plus-switch, mute-plus-switch, audio
capture, actual app background, physical queue disposal or shared-reader
lifetime acceptance. `metrics` describes the initial baseline;
`lifecycle_diagnostic.windows` contains the subsequent observations.

## 5. Execution boundaries

The helper is authored and syntax-checked; no runtime result is claimed by
this inventory. Coordinate a lab slot before starting a daemon, because the
browser and native investigations share the lab host. Use an owned source
extraction, report directory and exact process guards. Never change global
CPU affinity, device/trust/network policy, production applications or user
caches. Physical disk pressure requires actual measured disk I/O on owned
files; source `read_chars` alone cannot prove it.

Unit execution and final adversarial review remain deferred. The parent
owns final readiness, the human Fable pause, and handoff to the designated
merge session.
