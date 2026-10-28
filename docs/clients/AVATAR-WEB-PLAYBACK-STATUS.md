# Avatar web playback — diagnosis and merge status

**Status:** open · **Owner:** Codex, Avatar web playback investigation · **Updated:** 2026-10-21

Companion to [the playback surface contract](PLAYBACK-SURFACE-CONTRACT.md).
This ledger tracks Safari presentation faults and Avatar's processed Dolby
Vision delivery.

## Current state

The presentation observer repair is deployed in `c7d78b119`. The Intel
completed-picture encoder repair landed in [PR #1250](http://forge.lan:3000/noirr/plurx/pulls/1250)
and was deployed as `168fe74fb` on October 21. Paul confirmed enhanced
playback on Apple TV. Safari then exposed two downstream contract defects:
processed admission misread the original-DV transport requirement, and the
native readiness reader refused valid full-film playlists above 64 KiB.
The follow-up corrects those shared contracts; Paul retains deployment.

The original three Safari drops are now located in retained macOS media
logs: they occurred after decoding, during two display-reporting intervals
that also contain screenshot/window activity. The exact presentation trigger
is still unproved. A separate pending-native-callback fault is reproduced;
no callback or dropped-frame correction is included in the DV follow-up.

## October 21 — processed delivery capability and complete timelines

After deployment, Safari's 18:47 UTC processed qualification took
2,123/2,065 ms and refused with `the processed recipe is not one this client
can decode`. The completed-picture timeout was no longer the blocker.
`processed_client_compatible` treated `dv_transport: "progressive"` as an
exclusive prohibition on HLS. Its documented meaning is an original
preserved-DV delivery requirement: `"hls"` forces that envelope; progressive
capability does not exclude another transport in `transports`. Safari
advertises both progressive and HLS; Apple TV requires HLS. The correction
retains HLS, MP4 and the same decoder entry's Main10/P8/PQ, raster, cadence
and bitrate checks. HDR10 output still requires the HLS `hvc1` claim.

An isolated Safari tab changed only the original-DV requirement to HLS,
leaving its decoder claims intact. The deployed server admitted FEL/RPU/DYN
in 1,834 ms, then native startup refused `Native readiness body exceeds its
byte limit`. `nativeHlsFetch` bounded both master and complete media documents
at 65,536 bytes. Avatar's processed timeline has 5,915 approximately 2.002 s
segments, about 183.5 KiB; its coarser copy timeline fits the former bound.
The client must consume the entire media timeline for seek-boundary evidence.

The follow-up gives successful media playlists a finite 4 MiB + 72-byte
readiness budget, using the retained-manifest budget and bounded acquisition
start-tag size as precedent. This is an explicit client support bound, not
an assertion of a universal producer duration limit. Actual master playlists
and HTTP error bodies retain 64 KiB bounds; initialization retains 1 MiB.
The initial URL may itself be media, so grammar identifies a master before
its smaller byte bound is applied. Full timelines and ENDLIST are retained;
overflow, reader cancellation, deadlines and ownership fences remain.

Pinned Rust 1.97.1 all-target check passed. The focused
`processed_profile81_caps_require_one_actual_main10_pq_raster_and_audio_contract`
regression passed once, including progressive/HLS/absent original-DV
requirements, missing actual HLS/MP4/P8/PQ and HDR10 sample-entry controls.
`node --test tests/playback/native-hls-startup.test.js` passed 38 tests,
including complete 5,915-entry child/scalar media, exact body limits,
overflows and cancellation. Explicit `gpt-6-astra` adversarial review approved
the implementation with zero blocking findings.

A final isolated-tab replay used the reviewed readiness reader and the
HLS-required original-DV claim against deployed `168fe74fb`, without changing
the server. It reported FEL/RPU/DYN, displayed the picture, decoded 3,010
frames with zero browser-reported drops, and delivered 2,988 accepted native
callbacks without manual rearming. Intentional pause/info interactions and
a prepared secondary element occurred during the capture. Startup took
24.9 s; initial seek-to-loadeddata accounted for 22.3 s and remains a latency
investigation, not passing fast-start evidence. Display fidelity/HDR output
is unqualified. Diagnostic progress reporting was suppressed and the tab
closed; this is injected-client evidence, not deployment acceptance.

## October 21 — original dropped frames recovered from system logs

The original application traces retained no nonzero drop transition. macOS
unified logs do retain the hardware decoder and image-queue reports for the
same 4K Avatar item and resume timeline. Times below are America/New_York;
report endpoints are aggregate dump times, not individual dropped-frame times.

| Display reporting interval | Image-queue drops | Matching hardware decoder |
|---|---:|---|
| 11:14:31.507–11:14:37.524 | 1 | 144 decoded, zero dropped/late, 13 ms mean / 20 ms maximum decode |
| 11:14:37.524–11:14:43.530 | 2 | 144 decoded, zero dropped/late, 13 ms mean / 22 ms maximum decode |

Both intervals report no compressed-frame-queue starvation, image-queue
flush, eviction or post-sampling drop. Display refresh stays 16.667 ms.
Later reports through 11:15:55 contain no additional drops. This locates
these three losses after decoding; it does not establish a server timeout,
callback omission or decode-throughput cause.

The first interval contains native screenshot selection beginning 11:14:33
and capture at 11:14:36.772–36.783. The second contains Codex foreground and
window changes around 11:14:38.934, CoreAnimation image-function errors, and
a computer-use window screenshot at 11:14:43.486–43.502. These are specific
presentation disturbances, but the aggregate reports lack individual
missing PTS/reasons. A controlled capture replay has not established
causality: a targeted shortcut/drag selected page text, and opening native
Screenshot controls timed out. Neither attempt is a successful reproduction.

An independent passive native-boundary replay also reproduced zero callback
delivery while raw decoding advanced with zero browser drops. Its first
request was armed at loadeddata/readiness 2, 1 ms before seeked, and remained
pending while focused for about 8.5 s, then through automatic recovery.
Renewing the successor's request yielded a callback 20 ms later and 834
accepted receipts over 35 s. Focus returned only 5 ms before renewal, so
renewal alone is not yet isolated. This confirms native silence before app
filtering, but it cannot explain or be counted as the original three drops.
No observer patch is included. Private raw JSON and system-log receipts are
retained outside the repository; no media or private infrastructure is added.

## October 21 — Quick Sync preference selected software FEL output

At 13:47:44 UTC, the server declined Avatar's processed Auto trial with
`DV private window deadline expired`; the qualification phase took 5,046 ms.
The 15:14:15 UTC reopen reused the same failed episode and immediately
declined processing. Ordinary P7-to-P8.1 delivery retained dynamic metadata
and discarded the FEL. The episode exclusion explains the later message;
it does not identify the original slow stage.

A private, read-only original-source window at 4462.458 seconds isolated
the stages on the affected Intel server, using the installed media/helper
bundle and a fresh shader cache for every trial:

| Graph | Observation |
|---|---|
| Decode and reconstruct 48 native 4K pictures; NUT output discarded | 1.019 s; VAAPI decode, Intel Vulkan, 15 preroll pairs, 48 FEL contributions |
| Same reconstruction into the existing single-thread x265 recipe | Encoder did not finish within the 30 s diagnostic bound; renderer itself was backpressured until 8.7 s |
| Same reconstruction into Main10 VAAPI with P010 upload | 1.734 s; 48 pictures, both processes exited successfully |

These isolate the encoder bottleneck, not complete serving acceptance: the
comparison omits soundtrack encoding, MP4 publication and RPU adaptation.
The original failed run's encoder command/receipt was not retained. Its
attribution to software encoding is inferred from the selection logic and
these stage measurements, rather than a causal replay of that failed create.
`completed_window_encoder` accepted preferred NVENC or proved VAAPI, while
the affected server's preferred Quick Sync fell through to software. Its
x265 graph deliberately permits one frame thread with no pool or wavefront
parallelism. The fix selects the existing VAAPI completed-output contract
for either Intel preference when the host-level VAAPI HDR10 passthrough
probe and reconstructed contract pass. That probe is not bound to a render
device or the reconstructed P010 upload path; actual output validation is
still required. Multi-GPU/device-bound proof remains open. Explicit software
selection, absent-proof fallback, admission,
encoder identity and the startup deadline retain their existing contracts.

Focused evidence with Rust 1.97.1 on Linux:
`cargo test -p plurxd --bin plurxd reconstructed_intel_preferences_use_only_proved_vaapi_output -- --exact dv_segment::tests::reconstructed_intel_preferences_use_only_proved_vaapi_output`:
one passed. The regression covers both Intel preferences, the positive and
negative proof, software/NVENC controls, three rasters and four cadences.
Independent Astra review found no actionable issue in the source diff.
Rust 1.97.1 workspace Clippy, formatting, catalog lint and served-script
syntax passed on macOS; Linux daemon all-target Clippy and six documentation
index checks also passed.

The production `RuntimeRecipe` code path under the private physical harness
then passed consecutive 48-picture windows
2229 and 2230 at 4462.458 and 4464.460 seconds. The cold window took 1,739 ms
and the adjacent window 1,299 ms for 2,002 ms of media each. Both used
40 Mb/s Main10 VAAPI, actual TrueHD-to-stereo-AAC/48 kHz/256 kb/s, 48 FEL
contributions and 48 fresh/adapted Profile 8.1 RPUs. Production authoring,
encoded-window/RPU/grid validation and child retirement passed, with a
stable processing plan across the windows. These are private runtime/output
checks, not independent picture-fidelity or enhanced display acceptance.
The harness configured an 8 s preparation block budget and an 8 s execution
deadline per window. This is distinct from the production create's dynamic
qualification budget. The measured windows achieved 1.15× and 1.54× realtime;
they do not establish sustained playback headroom. Hardware evidence here is
limited to the measured single-render-node Intel configuration.
The ignored physical harness and MP4/scalar evidence remain outside the tree;
earlier diagnostic setup/reporting failures are not passing evidence.

At the user's explicit request, additional Fable 5.1 review of frozen head
`78bbc61ab` through Hermes task `t_7603a52c` approved the source change with
zero blocking findings. It verified the fall-through, output policy and
decoder/encoder independence. Its non-blocking proof-scope and evidence
wording findings are clarified above; device-bound proof and the pre-existing
unconditional NVENC contract selection remain tracked in the
[selection reference](../streaming/DV-SELECTION-PATHS.md#open-proof-and-contract-work).
The private review recomputed the retained log/harness hashes; it did not
re-execute media or qualify display output. Required gate run 5700 passed on
that frozen head; later documentation/comment changes require their own gate.

The viewer also reported three Safari dropped frames. Their original event
times were not retained, and they are not evidence of the earlier server
timeout. A subsequent 2,937-frame replay reported zero drops. A fresh-page,
counter-only 95-second replay then reported 2,263 frames, zero drops, no
hitch or metadata faults, and 24 fps. A 120-second replay with the Details
panel open and scrolled to Enhancement/Frames reported 2,892 frames and
zero drops; the slowest measured `updateStats` call took 5 ms. These runs
do not establish the cause of the original three drops. The first
diagnostic replay is excluded: its raw callback was registered before media
readiness, reproducing the known
[Safari callback-stranding mechanism](../playback-control/CALLBACK-ROOT-CAUSE-AND-FIX.md)
in the instrumentation itself. No frame-drop correction is claimed from the
invalid capture or the later healthy runs. A final counter-only replay at
4494 seconds (original startup segment 545) instead exposed zero presentation
callbacks while decoded frames advanced, followed by exhausted automatic
recovery. The stored plan and a bounded source-packet check independently
confirmed that entry 545 starts with a clean IDR (NAL type 19) at 4489.902 s
and no leading pictures, ruling out initial CRA/RASL discard for these drops.
Its replacement decoded 456 frames with zero drops; the probe
registered no native frame callbacks. This separate callback-lifecycle fault
remains under investigation. Diagnostic playback closed and the saved resume
point remained at 1h 16m.

The verification build exhausted the server system volume and caused a
container restart. Task-owned compiler caches were moved to temporary
storage; the server returned healthy with 8.8 GB free. This operational
interruption happened after the playback captures and is not evidence about
the original drops. Production image/settings were not upgraded by the probe.

## Earlier presentation observer repair

The observer repair and regression coverage are complete. [PR #1247](http://forge.lan:3000/noirr/plurx/pulls/1247)
is owned by Codex for this investigation and carries the current gate and
merge state. The single adversarial review of `fa492e1` found no actionable
issues; focused evidence passed. The original incident's exact timestamp
trigger and deployment remain distinct from the proven observer defect.
Lower model GPT-6 Luna completed bounded source tracing and regression work.
Review and test execution followed implementation, as requested.

## Incident evidence

At 01:24:34 UTC on October 21, Safari attached Avatar: Fire and Ash through
native HEVC HLS on lab6. The server reported ready playlists. At 01:24:45,
the client declared a presentation stall despite 56.4 seconds buffered. It
reopened at 01:24:57; the replacement buffered 51.2 seconds but again reported
no presentation, then exhausted recovery at 01:25:18. No Play permission
rejection was recorded. The viewer reported moving video under the overlay.

## Confirmed implementation defect and incident limits

The viewer confirmed that the picture visibly moved under the overlay.
`armHitchDetector` discarded callbacks with suspicious future media timestamps
before incrementing `controlPresentedFrames`. The existing progress sampler
requires both the element clock and that count to advance. Thus a diagnostic
metadata filter could turn real delivered frames into false no-presentation
evidence, retain a buffering surface, and spend the automatic recovery budget.

The repair counts current owned callbacks before filtering their timestamps.
Suspicious timestamps still cannot settle a seek or enter hitch/rate history.
Startup presentation uses the existing element-clock-plus-callback proof.
No fallback signal, recovery timer, or configuration switch is added.

Same-tab and cold-load Safari resumes during diagnosis delivered about 24 fps
and cleared their startup surfaces normally. A bounded visibility experiment
also retained frame callbacks while hidden and did not report a stall.
The original failed attempt did not retain its hitch/metadata counters, so
its exact trigger is unconfirmed. The regression must prove the identified
counter defect without claiming the original physical failure was reproduced.

## Review and focused evidence

The adversarial reviewer checked the diff, touched outlines and this ledger.
No actionable findings. Ownership guards remain before receipt counting, and
seek settlement still requires valid timestamps and current identity/epoch.

`node --test tests/web/hitch-lifecycle.test.js`: **7 passed**, October 20.
The production detector and progress sampler retain presentation for 20 seconds
of moving video with future callback timestamps, retire a media wait, and do
not enter persistent recovery. Missing callbacks and a frozen element clock
still enter stall handling. Invalid PTS cannot enter seek or hitch evidence.
This is a deterministic callback-metadata regression, not a reproduction of
the original Safari attempt.

Browser tracing was removed; diagnostic playback was closed and the resume
point restored conservatively to the original displayed 1:13.

## Safari fault injection

Native Safari playback with callback media timestamps offset by one second
reproduced the complete preparing/buffering, one restart, and exhausted prompt
sequence over moving video. At 1:13:14, 338 callback timestamps were rejected
and zero receipts credited. Loaded media remained plentiful.

With the reviewed receipt ordering applied in memory on a fresh player under
the same injected timestamps, playback remained healthy past that interval:
297 receipts/anomalies at 1:13:51, 849 at 1:14:13, and 1,207 at 1:14:28.
There was no active wait, no recovery spent, and the loading overlay was hidden.
This confirms the mechanism on real Safari frames with synthetic timestamp
faults; the original attempt's callback metadata was not retained.

All injected code and temporary tracing were removed by restoring the native
callback API and reloading the page. Diagnostic playback was closed; the
visible resume point was restored to 1:13 and Web Inspector hidden.

## Remaining acceptance

The PR records current fast-lane and merge results. Deployment and an
uninstrumented replay of the original failure remain separate acceptance;
healthy resumes alone do not prove which original callback metadata occurred.

