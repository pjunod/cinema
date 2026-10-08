# Continuous-quality matrix — what the retained receipts establish

**Status:** incomplete · **Inventoried:** 2026-10-08 ·
**Candidate baseline:** `f1a71d136` ·
**Latest measured server:** `909e0c7b7` · **Prior server:** `67520f7a8`

Companion to [CONTINUOUS-QUALITY-BUILD.md](CONTINUOUS-QUALITY-BUILD.md#83-acceptance-is-a-per-switch-result-not-an-average).
This inventory separates current component evidence from older or missing
source/lifecycle/pressure rows. It does not replace the build ledger or
qualify a newer integrated candidate. The parent session owns integration
and the final platform-wide acceptance decision.

## 1. Read source and scope before a pass

This update parsed 615 top-level JSON files for report source/outcome; 185
contain `server.build`, and exactly one explicitly attests `909e0c7b7`.
The original 192-report inventory and its historical hashes remain intact.
"Absent" means no qualifying receipt was located in the inspected set or
ledger, not that none can exist elsewhere. A filename does not attest source.
Native 236 is bound by its launch controller and exact-source build ledger;
its SDK proxy report has no `server.build` field. That distinction stays in
the companion JSON. Separate device evidence cannot transfer between platforms.

A current scoped pass proves only the observation described in its row.
A safe retained-current result is not a successful quality transition.
Twenty consecutive completed transitions, including five actual Auto changes
where supported, are still required for each platform and family. A failure
ends its series; no diagnostic helper aggregates retries into a pass.

## 2. Source, lifecycle and pressure rows

| Required row | Evidence classification | Observed | Still required |
|---|---|---|---|
| Desktop Firefox / normal / 1x | prior-source scoped pass | Source 67520f7a8: fifteen manual and five actual Auto transitions; one session/player; zero reported hitches/stalls. | Callback/transport only; normal/fullscreen and mute state are not separately attested. Full pixel/audio acceptance remains open. |
| Headed Firefox / capture campaign | prior-source failed | Source 67520f7a8: Fifteen manual; p95 video gap 169.62 ms > 100 ms; Auto absent. Capture incomplete: 221 holes, 76 skipped counters; upper hold 299.988 ms. | Keep failed. Browser optical session owns diagnosis; this inventory does not erase it. |
| Desktop Chrome | older evidence | Earlier full campaigns described in build ledger; no 67520f7a8 Chrome report among 192 parsed playback reports. | Current-source full campaign and independent display/audio evidence. |
| Fullscreen | prior-source scoped pass | Explicit server.build67520f7a8: trusted shipped fullscreen control, actual #player normal/full/normal; three eight-second windows 0.997545/0.997550/0.998229x; max retained frame gap 84.94ms; End counts 2/0/0/0. No quality requests. | 909e0c7b7 state repeat remains unmeasured. Switch in each presentation state and independent physical/pixel/audio evidence still required. |
| Muted / unmuted | absent explicit row | Current continuous fixture has AAC, but presence of audio is not mute-state or captured-audio evidence. | Existing mute-cycle helper supports explicit muted/unmuted state windows, unrun on909e0c7b7. Switching in both states and independent unmuted audio capture remain open. |
| Subtitles | fixture/tooling prepared; playback absent | Owned caption remux proof is verified: 450 deterministic text cues; all 43,200 video and 84,376 audio packet payloads/timestamps/time bases equal the incumbent, with source/output file hashes. Shipped selector Off/On/Off helper is authored; no subtitle playback receipt inspected. | Run bounded text cue/clock/identity diagnostic after pause lane; retain remux proof. Cue metadata cannot prove visible pixels. Subtitle-plus-switch and physical/audio remain open. |
| 0.5x rate | prior-source mixed/failed | Source 67520f7a8 no-D3 short pixel capture passes 95.876 ms upper hold; same report fails one callback hitch. D3-enabled current capture fails 162.572 ms. No quality request in these windows. | Keep every result scoped; current rate-plus-switch and audio evidence absent. |
| 2x rate | older steady pass | Source eab5fceb8: measured 2.00546× over 12 s; zero reported stall/hitch/reopen. | Existing current helper supports one rate-2 state/clock window, unrun on909e0c7b7. Quality transition in rate window and audio evidence remain open. |
| Long prebuffer | older/mixed diagnostic | Older eab5fceb8/0e778892d source prebuffer diagnostics and full campaigns have buffering observations; no explicit current long-prebuffer acceptance row. | Record retained frontier/runway, delayed target boundary and bounded physical work without treating preparation expiry as presentation expiry. |
| Rapid choices | prior-source scoped pass | Source 67520f7a8: 720→480→720 final 720p presents in 4,056 ms; same session/player/family; zero reported hitches/stalls. | Focused lifecycle evidence only; no pixel/audio acceptance. |
| Pause / resume | current failed | Explicit server.build 909e0c7b7: eight-second pause keeps clock/identity; resume reports one held counter. Subsequent eight-second moving window 1.0032975x/gap 83.78ms has no window errors; End counts 0/0/0/0. Overall failed: whole lifecycle hitches 1. | Parent owns held-on-resume diagnosis and focused changed-code check. Clean moving window or End must not erase the retained failure. Pending-switch pause/resume and physical/audio remain open. |
| Seek collision | prior-source scoped pass | Source 67520f7a8: Final 720p presents in 2,756 ms; same session/player/family; zero reported hitches/stalls. | Separate receipt because harness deduplicates identical fixture/quality/operation names; no physical acceptance. |
| Background / foreground | absent actual lifecycle row | scripts/playback-lab suspend-resume calls media.pause()/play() and labels surrogate=paused-media-element. | Actual app/tab visibility lifecycle and restored control ownership required; paused media is not background. |
| Explicit End | prior-source process-scoped pass | Source 67520f7a8 full headless campaign producer_end samples have zero child FFmpeg processes at 0/1/3/5 s. | Does not prove physical queues, all private reader leases or pending callbacks have retired. |
| Natural end / tail switch | older scoped pass | Source 97967c6ce: start 1785 s; target 720p at 1792 s; natural EOF 1800 s; zero End producer counts. | Current tail. Target 1792..1794 artifact was not copied, so independent target join binding absent. |
| Real high-bitrate title | absent | Inspected current fixture is synthetic MPEG4/AAC 1080p, 4.2 Mb/s. | One authorized real title with source identity, measured bitrate/decode load, compatible family and exact receipt. |
| Low-end device | absent qualifying physical row | No low-end-device acceptance established by inspected current desktop reports. | Physical low-end runtime; software emulator is a separate scope. Coordinate with platform owners. |
| VFR / nonzero source origin | absent | Current source fixture is 24 fps CFR. | Measured source shape and actual family eligibility; truthful unsupported result if normalization cannot satisfy invariants. |
| Source-copy / HDR / HEVC | absent current qualifying row | These are distinct media compatibility cases in §5.1; CFR AVC passes do not transfer. | Separate exact-family eligibility and switch/boundary results. Do not force unsafe enrollment. |
| Decoder-limited case | older intervention only | 087e27680 explicitly co-locates source decoder and target on one CPU; incumbent retained then Retry works. | Separate from healthy switch acceptance. Current device/decode-limited case absent; preserve foreground thread policy. |
| Network cliff / recovery | prior-source scoped pass | Source 67520f7a8 full Firefox campaign has five actual Auto transitions with catalog-derived shaping. | Callback/transport evidence only; physical output and other source/device cases still open. |
| Occupied foreground pool / Retry | older intervention pass | 75c9c7ba3: two viewers; occupied 9 credits; limit lowered32→9; target refused safely; restoring limit permits shipped Retry. | Current-source normal-load qualification absent. Explicit budget intervention is not physical pressure. |
| Target CPU / source-read contention | older scoped pass | 087e27680: competing decode and target active on one CPU; observed read_chars; both read_bytes deltas zero. | Current-source contention absent; no disk-pressure or shared-reader-fairness claim. |
| Shared-reader fairness | absent | Existing two-viewer pressure pass does not measure ownership/fairness of a shared source reader. | Concurrent reader identity, progress per consumer, cancellation of one without starvation of the other. |
| Physical disk pressure | absent | Retained CPU/read probe recorded zero physical read_bytes. | Bounded owned-file I/O with actual physical read/write evidence and incumbent continuity; no user caches or global settings. |
| Safari / iOS / tvOS / Android matrix | separately owned/incomplete | Platform owners retain native receipts and failures; desktop evidence cannot qualify these rows. | Obtain their source/device-specific runtime evidence through parent; full acceptance remains open. |
| iOS normal-selection full series | current method-path full-series failed | Lab 236 fresh namespace binds server 909e0c7b7 through launch controller/build ledger; SDK receipt has no server.build field. Fifteen manual requests each observed; 346 playback probes; zero actual Auto changes; series terminal native_failure/app_script_stopped. No UI taps or physical/audio capture. | Parent owns Auto failure diagnosis. Fifteen manual observations do not satisfy fifteen-manual/five-actual-Auto acceptance; no twenty-transition or platform-readiness assertion. |

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

Fresh SHA256-bound receipts are `firefox-909e0c7b7-pause-current1.json`,
`firefox-67520f7a8-pause-current2.json`,
`firefox-67520f7a8-fullscreen-current1.json`, and
`ios-236-series1-full-control-{sdk1,context1}.json` in the same report folder.
The native app receipt and launch controller live in the owned ignored
`target/cq-acceptance-runtime/` folder; their exact names and hashes are in
the JSON inventory. The caption packet proof and native final owned-process
guard are separately bound there; construction/cleanup evidence does not
change playback verdicts. Preserve failure status and source-binding scope.

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

The pause helper has the failed 909e0c7b7 receipt described above. Rate/mute
and subtitle state cases remain unmeasured; fullscreen has only the earlier
67520f7a8 scoped pass. Coordinate a lab slot before starting a daemon, because the
browser and native investigations share the lab host. Use an owned source
extraction, report directory and exact process guards. Never change global
CPU affinity, device/trust/network policy, production applications or user
caches. Physical disk pressure requires actual measured disk I/O on owned
files; source `read_chars` alone cannot prove it.

Unit execution and final adversarial review remain deferred. The parent
owns final readiness, the human Fable pause, and handoff to the designated
merge session.

## 6. Next software cases — use one named receipt per observation

The parent owns the pause failure lane first. After its justified change and
focused verification, run these cases one at a time on the owned lab. A
failure stays failed; do not rerun unrelated cases. Each helper refuses an
existing report path. Supply the actual new server build when source moves.
These commands are preparation only; this inventory ran none of them.

```bash
# Owned paths only; use the parent's source-only lab extraction and guards.
CQ_SERVER=/owned/source/target/debug/plurxd
CQ_BUILD=909e0c7b7
CQ_FIXTURES=/owned/fixtures
CQ_FIREFOX=/owned/firefox
CQ_GECKODRIVER=/owned/geckodriver
CQ_REPORTS=/owned/reports

# Priority 2: actual text stream, Off/On/Off; requires owned headed DISPLAY.
scripts/continuous-quality-subtitles --server "$CQ_SERVER" \
  --source-ref "$CQ_BUILD" --browser firefox --firefox "$CQ_FIREFOX" \
  --geckodriver "$CQ_GECKODRIVER" --fixtures "$CQ_FIXTURES" \
  --json "$CQ_REPORTS/subtitles-current-first1.json"

# Priority 3: explicit muted/unmuted state; no physical audio assertion.
scripts/continuous-quality-lifecycle --server "$CQ_SERVER" \
  --source-ref "$CQ_BUILD" --case mute-cycle --browser firefox \
  --firefox "$CQ_FIREFOX" --geckodriver "$CQ_GECKODRIVER" \
  --fixtures "$CQ_FIXTURES" --json "$CQ_REPORTS/mute-current-first1.json"

# Priority 4: eight-second 2x film-clock window; no quality request.
scripts/continuous-quality-lifecycle --server "$CQ_SERVER" \
  --source-ref "$CQ_BUILD" --case rate-2 --browser firefox \
  --firefox "$CQ_FIREFOX" --geckodriver "$CQ_GECKODRIVER" \
  --fixtures "$CQ_FIXTURES" --json "$CQ_REPORTS/rate2-current-first1.json"

# Priority 5: bind the earlier fullscreen state pass to the new exact build.
scripts/continuous-quality-presentation --server "$CQ_SERVER" \
  --source-ref "$CQ_BUILD" --browser firefox --firefox "$CQ_FIREFOX" \
  --geckodriver "$CQ_GECKODRIVER" --fixtures "$CQ_FIXTURES" \
  --json "$CQ_REPORTS/fullscreen-current-first1.json"
```

The subtitle helper needs the existing encoded-clock source. Its opt-in
recipe stream-copies video/audio, adds deterministic text captions, and
requires matching complete packet hashes, timestamps and time bases plus
source/output file hashes. It never recreates a missing incumbent source.
Keep the generated fixture's `.source-packets.json` proof beside it.

**How to read it:** these small cases can close only state/clock/identity
observations. They do not request quality, capture physical pixels/audio or
prove background lifecycle. Subtitle cues and their timing are metadata,
not visible-caption proof. End metadata classifies stable registered PIDs
without removing children or relaxing retirement failures. Existing tools
do not yet provide rate/mute/fullscreen/subtitle-plus-switch, actual tab
visibility, shared-reader fairness or physical disk-pressure acceptance.
Those rows need separate work; do not relabel surrogates to fill them.
