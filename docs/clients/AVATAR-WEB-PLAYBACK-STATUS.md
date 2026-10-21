# Avatar web playback — diagnosis and merge status

**Status:** open · **Owner:** Codex, Avatar web playback investigation · **Updated:** 2026-10-21

Companion to [the playback surface contract](PLAYBACK-SURFACE-CONTRACT.md).
This ledger tracks Safari presentation faults and Avatar's processed Dolby
Vision delivery.

## Current state

The presentation observer repair is deployed in `c7d78b119`. The October 21
enhancement investigation below identifies a separate encoder-selection
defect: a Quick Sync preference silently selected single-thread x265 for
reconstructed pictures despite a usable VAAPI output graph. The source fix
and focused Linux regression are complete. Complete private windows pass;
[PR #1250](http://forge.lan:3000/noirr/plurx/pulls/1250) carries the fix;
deployment and enhanced client/display acceptance remain open.

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

