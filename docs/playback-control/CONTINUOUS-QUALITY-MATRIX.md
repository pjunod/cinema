# Continuous-quality matrix — what the retained receipts establish

**Status:** incomplete · **Inventoried:** 2026-10-08, Android TV row 2026-10-10 ·
**Candidate baseline:** `f32594248d44a8f9a6ba9f1aad52b6d33909f1c9` ·
**Measured browser source:** `909e0c7b7` · **Prior browser:** `67520f7a8` ·
**Native diagnostic source:** `71d7f2ba4` ·
**Measured Android source:** `3201f925c` · **Current qualification:** pending

Companion to [CONTINUOUS-QUALITY-BUILD.md](CONTINUOUS-QUALITY-BUILD.md#83-acceptance-is-a-per-switch-result-not-an-average).
This inventory separates current component evidence from older or missing
source/lifecycle/pressure rows. It does not replace the build ledger or
qualify a newer integrated candidate. The parent session owns integration
and the final platform-wide acceptance decision.

## 1. Read source and scope before a pass

The earlier browser inventory parsed 622 top-level JSON files for report
source/outcome; 186
contain `server.build`, and 2 explicitly attest `909e0c7b7`. No report in
that set qualified its then-current `60c0f0f00` candidate. The Android
receipts added below bind `3201f925c`; they do not qualify the current
`f32594248d44a8f9a6ba9f1aad52b6d33909f1c9` tree. Its 57 statically
resolved regression fields are references, not executed regressions or
runtime qualification.
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
| Desktop Chrome | older evidence | Older campaigns retain their source scopes. Portable Chrome 155.0.8059.39 and runtime dependency readiness prepared; no60c0 Chrome playback receipt exists. | After failed source-path diagnosis is fixed, one exact-current Chrome baseline and required20-transition series with calibrated independent output evidence. Version/ldd readiness is not playback. |
| Fullscreen | prior-source scoped pass | Explicit server.build67520f7a8: trusted shipped fullscreen control, actual #player normal/full/normal; three eight-second windows 0.997545/0.997550/0.998229x; max retained frame gap 84.94ms; End counts 2/0/0/0. No quality requests. | 909e0c7b7 state repeat remains unmeasured. Switch in each presentation state and independent physical/pixel/audio evidence still required. |
| Muted / unmuted | absent explicit row | Current continuous fixture has AAC, but presence of audio is not mute-state or captured-audio evidence. | Existing mute-cycle helper supports explicit muted/unmuted state windows, unrun on909e0c7b7. Switching in both states and independent unmuted audio capture remain open. |
| Subtitles | 909 prior-source scoped pass | Explicit server.build 909e0c7b7: actual caption fixture and trusted Off/On/Off selectors; three eight-second windows 1.002252/1.002294/0.999273x; retained gap 85.58ms; actual cue timing/identity observed; End 0/0/0/0. No quality request or pixel/audio capture. | Keep909 scoped pass; do not repeat that unchanged scenario as a blind retry. New candidate and subtitle-plus-switch/visible-caption/physical-audio acceptance remain unmeasured. |
| 0.5x rate | prior-source mixed/failed | Source 67520f7a8 no-D3 short pixel capture passes 95.876 ms upper hold; same report fails one callback hitch. D3-enabled current capture fails 162.572 ms. No quality request in these windows. | Keep every result scoped; current rate-plus-switch and audio evidence absent. |
| 2x rate | older steady pass | Source eab5fceb8: measured 2.00546× over 12 s; zero reported stall/hitch/reopen. | Existing current helper supports one rate-2 state/clock window, unrun on909e0c7b7. Quality transition in rate window and audio evidence remain open. |
| Long prebuffer | older/mixed diagnostic | Older eab5fceb8/0e778892d source prebuffer diagnostics and full campaigns have buffering observations; no explicit current long-prebuffer acceptance row. | Record retained frontier/runway, delayed target boundary and bounded physical work without treating preparation expiry as presentation expiry. |
| Rapid choices | prior-source scoped pass | Source 67520f7a8: 720→480→720 final 720p presents in 4,056 ms; same session/player/family; zero reported hitches/stalls. | Focused lifecycle evidence only; no pixel/audio acceptance. |
| Pause / resume | prior-source failed | Explicit server.build 909e0c7b7: eight-second pause keeps clock/identity; resume reports one held counter. Subsequent eight-second moving window 1.0032975x/gap 83.78ms has no window errors; End counts 0/0/0/0. Overall failed: whole lifecycle hitches 1. | Parent owns held-on-resume diagnosis and focused changed-code check. Clean moving window or End must not erase the retained failure. Pending-switch pause/resume and physical/audio remain open. |
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
| Android UI13 Connect | source-bound setup failure | Server 3201f925c and owned emulator APK154: Connect failed; 63 Plurx nodes had no known manual/form controls. No Login, Play or quality request. A later read-only fresh-launch view exposed the manual control; the failed view's cause remains unmeasured. | Preserve the failure; do not count setup diagnostics as transitions or infer a production cause. |
| Android UI14 requested 480 | source-bound outside-family refusal | Server 3201f925c/APK154: initial 1080 and actual 720/1080 family were observed. Target 480 was absent; the helper stopped before quality input. Normal Signout and exact cleanup completed. | This is a scope refusal, not a 480 playback failure or a successful switch. Actual 720/480 coverage remains absent. |
| Android UI15 actual 1080→720 | scoped UI/SDK protocol pass | Server 3201f925c/APK154, actual 720/1080 family: normal shipped UI selected 720; actual target POSTs returned 200. New transaction accepted 720 revision 2/latest 2 with requested and accepted filmTick 912; SDK reported via=continuous quality=720 after 13,623 ms. Initial revision 1/tick 0 is excluded. Normal Signout failed; owned app force-stop and exact runtime cleanup were verified. | One emulator transition only. No 15-manual/5-Auto series, 720/480 pair, independent output capture, physical audio/display or newer-tree qualification. Preserve Signout failure separately from the observed switch. |
| Android prepared 1080 / first-prepared | retained-current failure; new diagnostics unmeasured | UI11 on 2314ccb45/APK153 requested 1080 outside its 480/720 continuous family. Successor media returned 200, but SDK retained the incumbent after 12,326 ms; no 1080 settlement. Later diagnostic source compilation is not a successful prepared handoff. | Qualify the prepared route separately from continuous family switches; current first-prepared/server correction remains unqualified. |
| Android TV physical · Auto after a viewer choice | scoped hardware pass of a root-cause fix | Source `991289a62`, capability-probe build 160 on a Google TV Streamer (API 34) against an owned lab daemon behind the shaping proxy. Manual 720p then Auto, and manual 480p then Auto: each time the Auto gate went `manual` → `change_pending` → `open` once the Auto entry committed `via=continuous`, and Auto then prepared and presented its own candidate. Before the fix, the gate stayed `change_pending` for the rest of the title (UI30). | Gate root cause only. Full 15-manual/5-Auto series on the device, an applied-cliff downgrade, and physical output/audio remain open. See [Build §10.303](CONTINUOUS-QUALITY-BUILD.md#10303-takeover-main-integration-and-auto-after-a-viewer-choice-2026-10-10). |
| Android APK156 | compile/install preparation only | Read-only Auto/EOF/frame diagnostics compiled with JBR 21; unit test sources compiled but were not run. APK SHA de71f37a5d0244c2a3f88ca21dc08884acc02564f8ade20177f95e37033637ef; 345 Android inputs and 3 generated reader assets matched parent 36fa91aba and frozen f32594248. Owned emulator installation/readback/idle passed in 7.329 s. | APK156 runtime is unmeasured; compile/install cannot qualify a switch, Auto journal or EOF probe. |
| Safari / iOS / tvOS / Android matrix | separately owned/incomplete | Platform owners retain native receipts and failures; desktop evidence cannot qualify these rows. | Obtain their source/device-specific runtime evidence through parent; full acceptance remains open. |
| iOS normal-selection full series | prior-source method-path full-series failed | Lab 236 fresh namespace binds server 909e0c7b7 through launch controller/build ledger; SDK receipt has no server.build field. Fifteen manual requests each observed; 346 playback probes; zero actual Auto changes; series terminal native_failure/app_script_stopped. No UI taps or physical/audio capture. Later lab238 on71d7f2ba4 reports 145 Auto fact rows and 79 SDK playback probes. Binding observer repaired, but 156 completed 200 segments have no link receipt; planned cliff did not apply; zero actual Auto changes. This diagnostic does not erase236 full-series failure. | First verify accepted Auto control/successor registration and one real applied cliff on new exact source. Only after that blocker is fixed start a fresh required20-transition series; no repeated15-manual diagnostic just to reconfirm prior success. |

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

The Android additions are separate from that historical JSON scan. UI13 and
UI14 retain their owned `cq-android-continuous-ui13/controller-result.json`
and `cq-android-continuous-ui14/controller-result.json` receipts under
`target/`; neither is promoted to a successful transition. UI15's safe
package is retained under
`target/cq-acceptance-runtime/android3201-ui15-evidence/`; archive SHA256
`d8de04cbbcc872c46c8d6822db058b2cbfa2f901be4aac20ead4c6d76443c4b2`.
Its controller, request/SDK proof and cleanup preserve the failed Signout.
The prepared-route failure is retained separately under
`target/cq-acceptance-runtime/android2314-owner-ui11-evidence/`.

APK156 preparation is retained under
`target/cq-acceptance-runtime/android-a9-apk156-preparation/`.
The source/provenance package SHA256 is
`048340af3ddb04c61cc0cc70b11c729191e7445091a1f6fefafcebab07d90794`;
its later `owned-install-result.json` SHA256 is
`11267ca84af406011e540dcb2fdea31fef891b12f1f9e011d3b32496503b8c98`.
The subsequent all-345-input/three-reader comparison also matches frozen
`f32594248d44a8f9a6ba9f1aad52b6d33909f1c9`; source equality is not runtime
acceptance. Installation used no server binary or playback lane. These receipts do not
update the historical companion JSON or establish current server/browser
qualification.

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

The pause helper has the failed 909 receipt described above. Rate/mute cases
remain unmeasured; subtitle state has the scoped 909 pass, and fullscreen
has only the earlier 675 pass. Both keep their measured source. Coordinate
a lab slot before starting a daemon, because the
browser and native investigations share the lab host. Use an owned source
extraction, report directory and exact process guards. Never change global
CPU affinity, device/trust/network policy, production applications or user
caches. Physical disk pressure requires actual measured disk I/O on owned
files; source `read_chars` alone cannot prove it.

Unit execution and final adversarial review remain deferred. The parent
owns final readiness, the human Fable pause, and handoff to the designated
merge session.

## 6. Small runtime batches — fix the failed path before a new series

The parent owns source freeze, exact compilation and the shared lab lane.
The current candidate is `f32594248d44a8f9a6ba9f1aad52b6d33909f1c9`.
The measured Android switch remains on server `3201f925c` and APK154;
APK156's later source/compile/install evidence remains separate. Current
first-prepared, integrated server and browser qualification are still open.
No prior component receipt transfers acceptance to the new tree.

| Order | Smallest useful batch | Evidence needed before moving on |
|---|---|---|
| 1 | Phone Auto registration and one actual cliff, after the diagnostic lane releases. | Accepted Auto control and correctly bound successor/body receipts, then an applied cliff with actual target progress. Native 238 repaired observer binding but observed 0 receipts and did not apply its cliff. Do not repeat 15 manual changes just to reconfirm them. |
| 2 | One actual 720→480 companion switch if the fresh session's family contains both; separately diagnose prepared 1080. | APK156 on a newly source-bound frozen lab; actual incumbent/family/member/recipe/cost proof before normal UI selection, then new transaction/revision/accepted filmTick and SDK continuous target. UI15 proves only 1080→720 on 3201; UI13/UI14 and prepared UI11 failures remain. No target tap outside the measured family. Only after focused success run the required fresh 15-manual/5-Auto series with actual accepted Auto changes and scoped client EOF/frame probes. |
| 3 | One mute-cycle state case and one 2x clock case. | Each fresh named receipt preserves identity/counters and End census. These cases remain unmeasured and cannot supply audio or quality-plus-rate/mute proof. |
| 4 | Required current-source browser20-transition series after focused blockers are fixed. | One frozen candidate;20 consecutive actual transitions including 5 Auto; independently calibrated output. Incorporate lifecycle states in that new series where supported rather than replaying old675/909 successes merely for reassurance. Existing state helpers do not yet implement plus-switch assertions. |
| 5 | Remaining platform/source/pressure gaps as separate scoped cases. | TV initial startup first; actual Safari native-HLS availability; separately authorized real/VFR/source-copy families and measured shared-reader/disk work. Do not relabel pause as background, an emulator as a physical low-end device, or read_chars as disk pressure. |

Fresh subtitle 909 state receipt passes Off/On/Off with actual cues and End
counts0/0/0/0. Keep that result under 909; do not rerun the same unchanged
scenario as a retry. Visible captions across quality switches and physical
audio remain unmeasured. The older fullscreen 675 pass likewise stays scoped.
When the candidate moves, the required final series must qualify its new tree.

```bash
# Prepared commands only: parent supplies exact new build and owned paths.
CQ_SERVER=/owned/source/target/debug/plurxd
CQ_BUILD=f32594248
# Full archive identity: f32594248d44a8f9a6ba9f1aad52b6d33909f1c9
CQ_FIXTURES=/owned/fixtures
CQ_FIREFOX=/owned/firefox
CQ_GECKODRIVER=/owned/geckodriver
CQ_REPORTS=/owned/reports

# One named mute state observation; no physical-audio or switch assertion.
scripts/continuous-quality-lifecycle --server "$CQ_SERVER" \
  --source-ref "$CQ_BUILD" --case mute-cycle --browser firefox \
  --firefox "$CQ_FIREFOX" --geckodriver "$CQ_GECKODRIVER" \
  --fixtures "$CQ_FIXTURES" --json "$CQ_REPORTS/mute-current-first1.json"

# One named 2x clock observation; no quality request.
scripts/continuous-quality-lifecycle --server "$CQ_SERVER" \
  --source-ref "$CQ_BUILD" --case rate-2 --browser firefox \
  --firefox "$CQ_FIREFOX" --geckodriver "$CQ_GECKODRIVER" \
  --fixtures "$CQ_FIXTURES" --json "$CQ_REPORTS/rate2-current-first1.json"
```

**How to read it:** a focused pass closes only its stated layer. Keep failures,
missing capture and unknown scope visible; no schedule in this document ran.
The separate purpose-scoped End observation may identify stable background
caption jobs, but never removes raw children or turns an existing strict
aggregate End failure into a pass. Reader pins, callbacks and physical queues
still need their own evidence.

## 7. Runtime prerequisites — ready to attempt is not playback acceptance

Portable Chrome 155.0.8059.39 has isolated version/dependency preparation;
no current Chrome playback receipt is claimed. Signed TV 239 compiled from
83 current production Apple files with only read-only QA diagnostics, and its
owned lab installation succeeded. It has no new startup, awake or physical
output evidence. Phone device info at 18:58UTC reports paired, connected tunnel
and DDI services available; no lock/unlocked or awake assertion follows from
that read. The companion inventory binds the filtered readiness artifact and
original source hash without copying private device metadata into this tree.

The Android UI preparation uses only the existing owned emulator and isolated
ADB server. Parent-created routing belongs to its exact disposable lab. Its
new backend/proxy templates require an explicit expected server source and
verify `/system` before UI entry. Normal Signout may need bounded Settings
scrolling and returns to the cached server's Login screen, so the next fresh
connection uses the shipped **Use a different server** control first.
These inactive preparations grant no runtime slot. APK156 installation has
completed on the owned emulator with the app idle; its playback and probes
remain unmeasured. No unit execution or current-tree acceptance follows.
