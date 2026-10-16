# Mac HDR acceptance — playback follow-ups

**Status:** open — ordinary F1 and bounded F2 are on main; bounded F3 current-owner controls, F4 parser/startup, F5 reporting, F6 media fence and F7 reference-white/package/Air evidence are complete on the effort; final current-tree promotion pending · **Recorded:** 2026-10-12; updated 2026-10-15.

The [full acceptance report](../streaming/MACOS-METAL-HDR-ACCEPTANCE-2026-10-12.md)
closes the ordinary HDR10 Metal/CPU comparison. This document specifies the
seven remaining playback and fidelity follow-ups, including what happened,
what the evidence establishes, what remains unknown, and how to close each.
It is the detailed follow-up plan for backlog A02, A04 and A06, rather than a
new implementation project duplicating their existing work.

## 1. Baseline, evidence and scope

The accepted Mac correction landed in
[PR #1038](http://forge.lan:3000/noirr/plurx/pulls/1038), merge
`0da656d3a3e7d7f0c52387543571d2ff4a265340`. The physical Air comparison used
`94dee81daf5fa2c3c400f873c05ee98df32f28d4`; the laptop was checked on
`f01b8605d`. Those identities are observation baselines, not claims about
what is installed when a future investigation starts. Record the current
main, installed daemon, browser and pinned media-tool identities before
attempting a reproduction. Source-only requalification of PR #1038 does not
make a later source tree physically tested.

American Gangster, local catalog file 15, supplied the Air's ordinary HDR10
case: 3840×2160, MaxCLL 1179, requested 1080p SDR. Heretic, local file 398,
supplied the laptop case. Catalog IDs are local reproduction hints; retain
source identity and selected-stream facts so another catalog can identify
an equivalent sample. Heretic's source/negotiation path must be qualified
separately from the ordinary HDR10 Air case.

### 1.1 Where the retained records live

- [Evidence manifest](../streaming/evidence/macos-metal-hdr-20261012/evidence-manifest.json):
  189 retained files, original checksums and storage locations.
- [Committed text receipts](../streaming/evidence/macos-metal-hdr-20261012/text-receipts.tar.gz):
  logs, measurements, scripts, failed attempts and qualification receipts.
- [Private screenshot/evidence archive](http://forge.lan:3000/attachments/8e191b9e-898b-4525-8cfb-77897f99c559):
  original captures attached to PR #1038; archive identity and checksum are
  recorded in the manifest.

Evidence member names below refer to those archives. The text copies remove
credentials and use neutral infrastructure names; screenshots retain their
original pixels privately. The acceptance report and evidence were committed
and merged in [PR #1043](http://forge.lan:3000/noirr/plurx/pulls/1043), merge
`eef013b5e2818b4018b25a8a138e1fd41677fa87`. Preserve the negative records when
adding successful reruns. A screenshot without `5m-presented` is not accepted
proof of the exact five-minute frame.

Times below are October 12 UTC unless otherwise stated; the local acceptance
session used EDT, UTC−4. Startup latency, playback position, seek latency and
server authority outage are separate quantities.

### 1.2 What is already accepted

Corrected Metal started in 2.363 seconds and played through five minutes with
zero interruptions and zero dropped frames out of 7,810. CPU started in
4.027 seconds. Buffered paused seeks independently proved presentation at
300,000 ms in 39 ms and 36 ms. Actual producer commands identify the Metal
and CPU graphs; browser intrinsic dimensions identify the 1920×1080 delivery.
The parser is installed and FFprobe identity binds on both Macs.

These successes coexist with the failures below. They establish neither
cold-start reliability on all sources nor uninterrupted quality switching,
unbuffered paused seeks, calibrated grade parity or complete UI metadata.
The original network-volume mount and service-access problems were repaired
operationally; do not presume they explain a fresh failure after the repair.

## 2. Work order and ownership

Priority here is a proposed investigation order, not an assertion that every
observation is a confirmed code defect. No new builder is assigned by this
document. The existing workstream that accepts a package records its owner,
current baseline, reproduction and disposition in its ledger.

| ID | Follow-up | First deliverable | Existing workstream |
|---|---|---|---|
| F1 | Unbuffered paused seek and stale-picture evidence | Reproduce with actual frame settlement and locate the first missing lifecycle transition | A04 lifecycle; A09 web seek |
| F2 | HDR remux → 1080p SDR quality change loses alignment | Explain the failed alignment receipt and distinguish safe refusal from an applied switch | A02 continuous quality |
| F3 | Abandoned preparation retains capacity | Trace terminal ownership and reservations; distinguish legitimate retention from failed cleanup | A04 lifecycle; A02 preparation |
| F4 | Laptop cold/remux startup unreliable | Matched cold/warm stage timeline for Heretic | A04 startup/recovery; A09 client playback |
| F5 | Delivered-rendition metadata incomplete or misleading | Join planned, published and attached facts without inventing actual renderer data | A02 presentation; A04 observability |
| F6 | Transient replicated-authority self-fence and 503 | Attribute the incident and separate admission failure from existing-viewer continuity | A04 serving/recovery |
| F7 | Residual Metal darkness and weak boot parity fixture | A pinned grade comparison and a diagnostic that detects the old peak error | A06 codec/GPU and HDR fidelity |

Start with F1–F3 because they affect ordinary interaction and admission, and
can share a source-bound trace. F2's failed preparation may be related to F3,
but the evidence does not establish a common root cause. F4 needs a separate
cold/warm experiment. F5 can improve evidence for all of them. F6 is a
recovered incident that needs attribution before any policy change. F7's
narrow regression-detection work can proceed independently; broader grade
qualification follows a declared reference contract.

Reconcile against the [lifecycle status](PLAYBACK-LIFECYCLE-STATUS.md),
[coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md),
[buffer observability contract](PLAYBACK-BUFFER-OBSERVABILITY-STATUS.md),
[continuous-quality status](CONTINUOUS-QUALITY-STATUS.html) and
[quality matrix](CONTINUOUS-QUALITY-MATRIX.md) before selecting code changes.
Prepared transitions, cancellation, generation fences and recovery ownership
already exist. A reproduction should identify the failing rule in that code.

### 2.1 Remaining execution claimed — 2026-10-15

Remaining F3–F7 work is claimed on `effort/hdr-playback-followups`, starting
from main `9eb7f6738647a7e963b17cfb3a2f77a594863c41`. Task branches target
that integration branch and require the Effort development gate; final
promotion requires a current-tree qualification receipt. Original evidence
and completed F1/F2 receipts remain authoritative for their bounded scopes.

| Task | Implementation and evidence ownership | Closure boundary |
|---|---|---|
| F7 neutral peak guard | `crates/plurxd/src/pipeprobe.rs`, this ledger, bounded evidence | Reject old explicit Metal peak units on real neutral PQ frames while retaining the 4K motion/speed probe. |
| F5 attached output facts | Frozen VOD status and web Playback info, associated regressions | Report attached output provenance without deriving an old rendition from current settings. |
| F3 lifecycle/accounting | Existing preparation/admission/maintenance paths and regression evidence; repair only a demonstrated gap | Distinguish the documented 300-second idle policy from a resource leak; protect unrelated viewers. |
| F4 cold Heretic | Isolated Mac playback namespace and source/stage evidence; repair the demonstrated failing stage | Repeated declared cold/warm starts, truthful delivery and bounded startup. |
| F6 authority recovery | Isolated serving-fence controls and retained stage evidence | Never inject faults into the shared fleet; preserve stale-owner refusal and distinguish process survival from actual continuity. |
| F7 visual fidelity | Pinned software reference, named source windows and display observations | Keep algorithmic differences separate from physical display measurements and artistic-grade equivalence. |

The operator has a Studio Display and Studio Display XDR but no measurement
instrument. Their factory calibration and Apple video reference presets can
support a documented visual comparison; no instrument purchase is required
for the software repairs. That comparison will be labelled as factory-reference
visual evidence, with no independent measurement of display calibration.
The original calibrated-fidelity criterion remains a distinct, unmeasured
boundary rather than being silently marked passed.

## 3. F1 — paused seek outside the local buffer stalls

### Observation and evidence

A paused seek outside buffered video stalled for roughly 20 seconds and
could reopen paused. A timeline showing 5:00 could still display an old
picture. The earlier `air-4e-metal-5m.jpg` and
`air-4e-metal-5m-info.jpg` are invalid exact-frame evidence: the displayed
picture remained from approximately 5:13 under a 5:00 timeline label.

Retained logs: `air-4e-final-playback.log`,
`air-4e-metal-startup-and-seek.log`, `air-94-metal-5m-proof.log` and
`air-94-cpu-5m-proof.log`. A resume-to-zero restoration also encountered an
unbuffered seek stall; the saved resume position was subsequently verified
as zero. Saving a position and presenting its picture are distinct outcomes.

The corrected buffered controls were:

| Path | Frame settlement time | Local buffered range before seek | Result |
|---|---|---|---|
| Metal | 22:11:52.543174, 18:11:52 EDT | 294,294–386,386 ms | `presented_frame`, landing 300,000 ms, 39 ms |
| CPU | 22:14:34.795471, 18:14:34 EDT | 298,795–382,382 ms | `presented_frame`, landing 300,000 ms, 36 ms |

Those controls prove buffered landing only. An empty interruption counter,
`currentTime` assignment or seekbar label does not prove frame presentation.

### Investigation

1. Repeat a paused, unbuffered seek on current source with one owned session.
   Record the accepted target, seek sequence, attachment generation, previous
   frame time, local ranges and server-ready range at the target.
2. Join the demand/control update, target segment request, producer readiness,
   response, append, decode callback and frame settlement on that sequence.
   Distinguish a genuinely missing segment from a distant buffered island or
   a stale snapshot. Find the first stage that fails to advance.
3. Check pause handling, demand issuance, loader state, attachment ownership
   and the existing recovery deadline. Determine whether recovery reopens
   the intended recipe and whether its callbacks belong to the new attempt.
4. Repeat with a superseding seek and a close during the pending seek. Old
   requests, callbacks and timeout actions must not settle the newer intent.
   Compare with the existing [web seek RCA](../streaming/WEB-SEEK-LATENCY-RCA-AND-FIX.md)
   before assuming this is a new defect.

### Acceptance and regression

Cover forward/backward, paused/playing, buffered/unbuffered and cold/warm
media rows. For each accepted seek, prove the presented target frame on the
right generation within the declared seek budget, or a truthful bounded
terminal failure. A successful paused seek remains paused at the new frame;
a superseded or closed operation cannot later reopen playback.

Declare target tolerance and latency budget from the current contract before
running the matrix. If the contract lacks a budget, propose and review one;
the observed 20-second wait is not an approved tolerance. Add a regression at
the identified demand/loader/settlement boundary, plus a real browser receipt
for frame presentation. A test that only updates the timeline is insufficient.

### Repair progress — both hls.js source repairs qualified and merged; HDR acceptance open

F1 is claimed by the paused-seek repair branch. On the October 12 main
baseline `6dc031387`, Pause suspends the established HLS loader with
`stopLoad()`. Local VOD seek execution then assigns `currentTime` without
restarting loading. An unbuffered paused destination therefore cannot arrive.
A later Play can also reject the old suspension because seeking advanced the
intent generation between Pause and Play.

The repair in `player/player.js` and `player/transport.js` grants target
loading to the current executed VOD seek on its exact media element and
attachment. It keeps the video paused, permits the owned playlist request,
and restores suspension after actual settlement. Supersession retires the
previous seek's loading and timer. The next Play can resume the newly held
loader; Play during loading leaves it running. Pause during a pending VOD
seek still permits that seek's target loading. A buffered target retains a fresh
suspension so Play before settlement can restart fetching. A paused deadline
with bytes but no target frame invokes the existing reopen; it cannot silently
retire recovery based only on coverage.
No Rust behavior, playback preference or feature switch changes.

The [repair receipt](evidence/paused-vod-seek-20261012.json) preserves both
negative and positive controls:

| Control | Observed result |
|---|---|
| Shipped seek regression before repair | 21 pass, one fails: Pause stops fetching and seek issues no target load |
| Focused seek/loader regressions after review | 43 pass; target-frame settlement, stale lease/attachment rejection, supersession, Play/Pause interleaving and deadline cleanup |
| Real Chrome, generated MPEG-TS, stopped-loader baseline | Target 35 s stays seeking, with no target frame or target buffer after 1,502 ms |
| Same browser/fixture with repaired loading | Unbuffered forward frame 35.000 s in 22 ms; unbuffered backward frame 24.080 s for target 24 s in 8 ms; both paused with seeking completed |
| Reviewed source, same Chrome fixture | Forward target frame in 19 ms, backward target frame in 5 ms; both remain paused and finish seeking |
| First browser harness | Preserved but invalid for final settlement: it accepted the callback before `seeked` and reused a buffered backward target |
| Fresh Air attempt on acceptance build `94dee81da` | Catalog reports analysis pending; original quality uses rolling remux. The rolling unbuffered reopen fails before media arrives. This does not qualify the repaired paused VOD path. |

Full `make web-unit-check`, web syntax and the unchanged TypeScript baseline
pass. The pinned Rust 1.97.1 baseline compiler check also passes. Reproduce the
interactive frame diagnostic with
`node tests/web/paused-vod-seek.fixture.cjs`, open its printed loopback URL and
press **Run diagnostic**. Set `PLURX_F1_RECEIPT` to retain its JSON outside the
temporary fixture directory. The diagnostic uses generated local media and
the shipped loading helper; the command-level regressions separately execute
the shipped seek path. Neither is a genuine HDR physical-display campaign.

Original HDR failures remain in the acceptance manifest/archive unchanged.
New Air failures and both browser harness versions are retained alongside the
repair. Astra found and verified the correction of a buffered-seek
Play-before-settlement race; three new regressions fail before that correction
and pass afterward. Local VOD PR qualification is complete. Remaining closure
work is the source-bound HDR-on-Air paused VOD matrix from this section.
Native HLS retains its separate implementation and qualification needs.
Before the October 13 follow-up, a forced reopen while still paused suppressed
its fresh startup manifest. The original local VOD deadline regression proves
a bounded reopen attempt; the new startup regressions below separately prove
replacement loading and terminal diagnosis at source level.

The local VOD repair merged in [PR #1047](http://forge.lan:3000/noirr/plurx/pulls/1047)
as `b2adc5f789f981e189ad72bfbe48383355b5b272`; run 4935 passed the current
candidate's policy, web, Rust and Main promotion gates.

The October 13 follow-up gives an executed paused hls.js replacement seek its
own startup loading lease. It can send the first playlist without Play and
complete startup on the actual target frame, then restore suspension. Its
original deadline remains active while paused; the real diagnosis raises
`startup_exhausted` with Close/Retry if no frame arrives. A suppressed or
interrupted unparsed playlist uses the existing single corrective reload.
Play carries a reserved reload to the current intent; Pause during the pending
seek preserves target loading. Supersession and cancellation fence old work.
Three initial regressions fail before the repair; 51 focused tests now pass.
Astra found and verified corrections to renewal, retry handoff and unparsed
supersession. The follow-up qualified on run 4953 and merged in
[PR #1053](http://forge.lan:3000/noirr/plurx/pulls/1053) as
`7f82b3894581cd3a2ba0955300dc39da9bfa9428`. Genuine HDR-on-Air acceptance
remains pending. Native HLS remains unchanged and is not newly qualified. The
existing repair receipt retains the source identities and failed controls.

**October 13 access blocker and restoration state.** The reviewed source
`5dabb22a3` is installed on the Air; both served player files match its
checksums. Its Metal boot probe passes at 29.05×. Cold and warm movie attempts
from the connected laptop browser fail at placement before HLS attachment;
these attempts provide no new physical Air frame evidence. A 16 MiB source
read and FFprobe succeed over SSH in 1.062 seconds. macOS records that the
updated `/opt/homebrew/bin/plurxd` does not match the existing Full Disk Access
code requirement. Renew the already-authorized access before another attempt;
SSH read success does not establish background-service access.

The local control Mac locked, and the Air screen connection requires fresh
approval. Physical captures and restoration await those manual steps. Original
backend: Auto; temporarily saved backend: VideoToolbox. Original Mac video
processing: on, unchanged. Shared clipboard was temporarily enabled; its remote
connection ended, but restoration to off is not verified. The movie's saved
resume position is still 34 seconds; the requested zero is not restored.
These are outstanding obligations, not acceptance results. The
[qualified source/access archive](http://forge.lan:3000/attachments/3df9990f-6607-4a8e-bbb5-8db7123b49d4) preserves the failed attempts,
permission diagnostic, checksums, review and qualification/merge receipts; its
SHA-256 is `5cf7a855b62ecf2ad59627737df5020712190cb3435cb71413212f9fe78569d7`. The first qualification attempt failed at
`spike-lock-check` because its runner could not resolve crates.io; retry 4953
passed. Original HDR captures and failures remain unchanged.

### October 13 physical rerun — renewed NAS access and paused surface completion

The renewed network-volume grant permits real Air playback. Source `5dabb22a3`
starts a fresh forced-1080p Metal session in 1,813 ms and runs continuously for
more than five minutes. Paused unbuffered local VOD seeks land on actual target
frames at 600,000 ms (194 ms) and 300,000 ms (157 ms). The CPU control starts in
3,816 ms and lands at 300,000 ms in 185 ms while paused. Actual producer commands
identify the Metal Hable/max/desat=0 graph with peak=117.9 and CPU Hable/desat=0
with peak=11.79; these use different peak units. Original screenshots and logs
remain retained, including a safely refused voluntary quality trial.

A forced paused reopen on that source presents the five-minute frame but leaves
“Preparing the stream…” over it for more than three minutes. This is a separate
missing surface transition after the repaired startup-loading lease. The new
regression executes the real surface reducer and fails on the previous code.
The correction publishes presenting only after current owned target-frame proof
successfully completes HLS startup. Stale epoch/attachment, off-target,
positioned-only and cancelled-startup cases cannot clear preparation. Both
seek/frame event orderings pass, and a later paused progress tick does not
resurrect the scrim. Astra approved the correction; 40 focused tests and the
full web checks pass, along with pinned Rust 1.97.1 workspace all-target Clippy,
formatting and served-script syntax.

The corrected physical acceptance binary uses a credential-free archive of the
healthy `5dabb22a3` baseline plus the exact reviewed patch `f7f8939d6`. Its SHA-256
is `9ebc6fc20e72a46b86da5fc5e86a57f0d9778c2f1f0a51c64eb10344ce52c6df`.
The served player matches the source digest
`42772cc914bc2c3cc198fd2e31ba4095cff66cf35df4d78b70a1ea3745ab6b07`.
This isolated hardware tree avoids unrelated contemporaneous main router
changes; the main-bound patch is separately integrated and qualified in
PR #1068. The installed parser retains digest
`36f7e0298eac14d1a664dfc083702bab812f6c83d196c651420af92f1080f21c`.
The corrected boot Metal parity probe passes at 2.26×. Its fresh Metal session
starts in 3,036 ms; an unbuffered paused local seek lands at 600,000 ms in
1,503 ms. Forced paused replacement at 300,000 ms completes HLS startup in
161 ms, clears preparation by the current presenting event, and stays paused
without Play. The retained screenshot displays the actual five-minute picture
and the Play triangle with no preparation overlay.

The capture transport is standard Screen Sharing, which carries SDR pixels.
These receipts qualify ordinary HDR10-to-SDR hls.js behavior; they do not
establish calibrated HDR-display fidelity, native HLS acceptance, Heretic
reliability, successful voluntary quality switching or the broader Mac matrix.
F2–F7 remain separate follow-ups. Preserve all prior failure archives and
source-qualified receipts; do not relabel a safely refused switch as applied.

**#1070 snapshot before the controlling Mac was unlocked:** PR #1068 passed Main promotion on run 4992 and merged as
`69d3311dd82e23aa869578442e9f1bc903cc6804`. The final CPU
capture and resume-to-zero await unlocking the controlling Mac. Clipboard
sharing is verified off; Mac processing is saved on and backend Auto is saved.
The earlier access/restoration block above remains historical evidence, not a
new observation.

The [October 13 receipt](evidence/air-hdr-paused-20261013.json) links the
[50-member raw archive](http://forge.lan:3000/attachments/4606c9d1-6bbb-4dda-a629-e6ffaacda016) (SHA-256
`5d200472b0b11b8774aa1a03f7dc829ce02541bb4349a4854c297e2bde209419`). It retains the original overlay failure, corrected
frame, safely refused quality change, local checks and qualification/landing
receipts. The snapshot is explicitly incomplete for the last CPU capture and
resume reset; no five-minute pass is inferred from elapsed wall time.

### October 13 completion — CPU control and restoration verified

The same corrected hardware binary completes the switch-off CPU control.
At 15:36:04 UTC, the intrinsic picture is 1920×1080 at film position
3,813.913 seconds, after 3,513.913 seconds (58½ minutes) of continuous play
from 300 seconds. The player reports zero stalls and zero dropped frames out
of 84,255. This is a player-counter receipt, not a calibrated frame-gap series.
While paused at 3,822.165 seconds with buffer 3,791.788–3,882.325 seconds,
the unbuffered backward target at 300,000 ms presents in 150 ms. Its actual
five-minute picture and Play triangle are retained without a preparation scrim.
Together with the Metal targets/reopen above, this closes the ordinary Air
Chrome hls.js acceptance of F1 and its reopened-startup/surface follow-ups.
Native HLS and the broader lifecycle/device matrix remain separate.

Cleanup is complete. The Air browser's quality returns to Auto through its
normal menu. Playback closes and its owned session is deleted before the exact
string item ID's progress is reset to zero; the authenticated read records
`position_ms: 0` and `watched: false`. After the service restart, the fresh
physical movie page offers **Play**, with no Resume. Backend Auto is active
and selects CPU/x264 at the startup benchmark (512 fps CPU, 359 fps
VideoToolbox); Mac processing stays saved **on**. Shared clipboard stays
**off**, with its prior functional transfer proof preserved unchanged.
The restored Auto backend intentionally uses the CPU tone-map, rather than
extending the temporary VideoToolbox acceptance preference.

The [13-member completion archive](http://forge.lan:3000/attachments/b6ecdca8-6885-4b71-883a-4e92bfc44cd2) retains the CPU soak and
paused frame, quality restoration, exact resume read, restarted Auto and saved
processing screenshots. Its SHA-256 is `5d412aed66cf5f2c3c69d2554f52c21704bb9849c3e982c7cc9d2a4605be9fa0`; size is 3912376 bytes.
The prior 50-member archive and every earlier failed receipt remain unchanged.
The previously pending state above is the #1070 snapshot; this completion
supersedes its outstanding capture and cleanup obligations. F2–F7 remain open.

## 4. F2 — manual SDR quality change loses alignment

### Observation and evidence

On candidate `94dee81da`, Air Chrome in its default HDR-capable profile was
playing American Gangster as 2160p HDR remux. A requested 1080p SDR change
failed at 22:01:11.464582, 18:01:11 EDT. Preparation
`6bdd65ef-6dd6-450b-b997-e88e1f16599d` reported:

```text
lost-alignment: phase=incumbent-frame video_callbacks=4 bad_frames=0
last_frame_age_ms=34 drift_ms=51 ready_state=4 buffered=true
```

The receipt also reports 17.1 seconds of runway. The optional manual target
failed and the incumbent HDR remux remained attached. That is retained
playback after a refused change, not a successful 1080p switch. See
`air-94-metal-startup.log`, `air-94-metal-startup-error.log` and
`air-94-metal-5m-proof.log`.

### Investigation

1. Reproduce the precise remux HDR → encoded SDR transition on current main,
   recording incumbent and prepared generation, source timestamps, media
   origins, audio timeline, browser profile and requested/effective recipe.
2. Identify which callback supplied the 51 ms drift, which clock it was
   compared with and whether both observations refer to the same instant.
   Trace frame freshness through inactive/prepared elements and any timestamp
   transform. Four callbacks and `buffered=true` alone do not prove alignment.
3. Locate the existing alignment policy and explain whether refusal was
   correct, caused by stale observations, or caused by incorrect mapping.
   Do not widen the threshold just because this sample failed at 51 ms.
4. Exercise cancellation and supersession, then inspect preparation teardown
   for F3. Keep the failure trace even if the next switch succeeds.

### Acceptance and regression

Add this source/path pair to the existing continuous-quality matrix. Include
playing/paused, visible/background, cold/warm artifacts and a superseding
change. Successful application requires the requested SDR rendition, frame
and audio continuity, and truthful applied-quality UI. A legitimate refusal
must preserve the incumbent and report one bounded, comprehensible failure;
UI metadata must continue to describe the incumbent.

Keep a regression for the demonstrated alignment or timestamp defect, and
independent frame/audio evidence for the physical transition. Existing Chrome
campaign successes remain valid for their own rows. A full restart only
qualifies an explicitly requested restart operation; it cannot stand in for
a claimed continuous transition.

### October 13 repair — compare both decoder callbacks

F2 is claimed by `codex/f2-prepared-alignment`, based on `016a0c59f`.
The physical Air reproduces the same HDR rolling-remux to 1080p SDR refusal
at 16:03:09 UTC. Preparation `21eaa26e-0495-4acd-a79f-60676410cff0`
has 21.5 seconds of runway, decoded advancing target frames and target
coverage. The rejected pair is incumbent film 17.976 seconds at expected
display 86,542 ms and target film 17.892875 seconds at 86,508.6 ms.
Projecting the target to that display instant leaves 49.725 ms of drift,
above the measured 41.709 ms frame cadence. Immediate exposure would be
unsafe. The retained October 12 refusal is not relabelled as success.

The first proposed repair kept observing until the existing 800 ms deadline,
but still compared only on incumbent callbacks. Its physical trial also fails:
preparation `b012715d-df09-4fa2-bb02-60bea3ae699d` receives 18 successor
callbacks before a truthful deadline refusal. Independent decoders deliver
their callbacks in different order; every incumbent-side comparison in that
trial uses the preceding successor picture. Preserve that failed proposal.

The revised repair caches the latest incumbent observation and evaluates the
pair when either decoder supplies an advancing picture. Both observations
must be at most 50 ms old. The decoded one-frame tolerance and original
800 ms deadline remain unchanged. Ownership, viewer intent, rate and target
buffer coverage are checked before exposure; the incumbent remains visible
and audible during proof. Nine regressions execute the shipped owner,
including an aligned later successor callback, sustained drift, expired
incumbent evidence, pause, supersession, and rate/buffer loss. Independent
review approves this revised source. At PR opening, physical qualification and
main-bound promotion were pending; the source landing below supersedes the
promotion wait. These source results do not close the physical matrix.

### Source qualification and landing — physical acceptance pending

[PR #1074](http://forge.lan:3000/noirr/plurx/pulls/1074) merged as
`96e4c49e92818f8f3e6b01e4786492b1c37a97d7` at 17:05:35 UTC.
Its qualified head is `fcac7a4ec6482d6f9b10aa39b02d390bc6771c78`, after
integrating current main `e4b6c1c6d`. Run 5015 (global 5041) passed policy,
web, Rust and Main promotion; matching preflight receipts were downloaded.
All nine regression references are in the landing message. Current-candidate
local compiler, Clippy, web-unit/static and docs checks also pass.

The [source receipt](evidence/f2-decoded-alignment-20261013.json) binds the
installed binary digest, served JavaScript digest, review, negative controls,
qualification and landing. Its private
[31-member qualification archive](http://forge.lan:3000/attachments/f6639fb2-2bb0-475a-b066-c7a24c79f967)
has SHA-256 `24895b8ff899e0002fa7c3e1815d3e8d51992a0a53372a6d5ec013b334ebb4f0`.
The separate [18-member failure archive](http://forge.lan:3000/attachments/09349205-d671-4076-b585-7f1c5335bb2f)
has SHA-256 `1d47578cc247e4016d5b7e9352c3303bb9519077a22723ce23fc582cd975690a`.
The original three HDR logs and that failure archive were rechecked unchanged.
The rejected immediate pair and failed wait-only proposal remain failures.

The qualified binary is installed and the service answers ready. After the
Air was unlocked, the installed-source attempt at 17:38:43 UTC failed before
decoding: macOS denied the NAS source open with `Operation not permitted`.
FFmpeg exited before `moov` or any frame; this is not an F2 alignment outcome.
The existing Plurx Full Disk Access row shows ON, but refreshing that grant
requires local Touch ID/password. The new
[four-member startup archive](http://forge.lan:3000/attachments/22ebe6e5-f5cd-4baf-a10a-26c3d5dc39dd)
has SHA-256 `59555bbf49760a37c1845db742e6b7141769a8b3099212bdb67498965d3ed20f`.
It preserves both fresh logs and the failed player/access-row screenshot.
No installed-source transition success,
audible seam, or applied-quality pass is claimed. The next receipt must run
the actual HDR remux → 1080p SDR prepared transition on this served source,
retain actual frame/quality and audio-ownership facts, and distinguish any
unmeasured physical audio output. Close the owned player, restore client
quality Auto and the exact movie's resume to zero, and verify the fresh page.
At 17:49 UTC the owned failed player was closed, client quality Auto was
verified, and the authenticated item read returned `position_ms: 0` and
`watched: false`. A fresh movie page offers **Play**, with no Resume.
The [restoration archive](http://forge.lan:3000/attachments/dfd38c6f-aba3-4109-bc41-209eb829a05a)
has SHA-256 `5d931248692a957e71ec577a8315557861b28410ddb9fc72e03a04723d6b922c` and preserves the API receipts and page.
Shared clipboard remains off; backend Auto and Mac processing on were not
changed by this source correction. Cleanup is complete for these attempts;
a subsequent physical run must restore the same state again.

### Installed continuation — HDR passes; SDR needs clock correction

Renewing the existing Plurx access entry against the installed binary restored
NAS playback after user authentication. At 19:47:05 UTC, preparation
`0d85d803-202f-49ef-b67e-7c52b2e40465` committed a 1080p successor on the
installed #1074 source. The fresh decoded pair differs by 41.667 ms within
41.708 ms cadence; the first visible frame follows commit by about 30.1 ms.
Pinned FFprobe confirms HEVC, 10-bit BT2020/PQ: this transition retains HDR.
It proves the two-sided rendezvous can commit, but is not an SDR pass.

The explicit SDR diagnostic at 19:56:53 UTC uses a temporary successor-only
SDR capability request after the HDR incumbent starts. FFprobe confirms an
H264 1920×1080 BT709/AAC successor. Preparation
`e203a2a2-2a27-4352-b4f4-a623ac9d7281` receives 19 advancing callbacks but
refuses at the unchanged 800 ms deadline. At one common display instant the
incumbent is at 9.968 s and successor at 9.884875 s: an 83.125 ms gap exceeds
the measured 41.709 ms frame cadence. The incumbent correctly remains visible
and audible. A warm retry refuses under incumbent pressure before frame proof;
that is a separate guard outcome, not another alignment measurement.

The remaining source defect is in clock correction: the 250 ms coarse window
skips seeking an 83 ms gap that the decoded guard must reject. Branch
`codex/f2-clock-alignment`, based on `4366a32dc`, limits initial and postseek
clock slack to one measured incumbent frame (capped at 250 ms), keeping the
coarse fallback when cadence is unknown. Decoded tolerance, freshness,
ownership, rate, buffer and audio guards remain unchanged. Two regressions
execute the shipped commit/alignment functions: correct the initial two-frame
gap, and correct a first seek that still trails by 60 ms. The first rejects the
unchanged owner; all eleven focused tests and full web units/static checks
pass. Astra approves the source. Qualification and installed SDR proof remain
pending; no SDR acceptance is claimed from these tests. The first qualification
run (global 5071) fails on an existing Android diagnostic consent-focus
adapter missing from the input fence. The correction scopes only its import
and existing handler; thirteen fence tests prove neighboring media handlers
still fail. Astra approves that ancillary correction. The failed run is retained.

The [24-member physical archive](http://forge.lan:3000/attachments/27798e53-f80c-4897-8a90-961253559255)
has SHA-256 `97f431a4ab2dafadde66de58be6a02642303477518870df4cb6a13e0dd6abfa5`.
It retains the NAS renewal failure, HDR pass, SDR refusal, warm pressure refusal
and clock regression controls. Physical speaker continuity and the
rate-limited control measurements remain unmeasured. At 20:10 UTC the player
was closed, the temporary capabilities removed by reload, quality Auto and
persisted resume zero verified, and shared clipboard OFF verified. A new
installed-source trial must repeat cleanup.

### Final clock correction — qualified, merged and installed SDR accepted

PR #1083 merges as `f1bffab5963399f084db2f94b158412d5cb8d9d2` at
21:23:44 UTC. Exact candidate `d1b480b5d5bd0ef7f9f6bad8ce9133bdcef05fa9`,
based on `b35eebb6b28c9f94b0a174c145953f79f39c1c8f`, passes run 5051
(global 5077): policy, Rust, web, Android and Main promotion are green;
Apple is outside the selected scope. Final Python receipt 2796 is complete,
with 1,385 applicable passes and no fixture errors; preflight receipt 2797
has no phase errors. All three regression lines are verified in the landing.
The intermediate missing client-history anchor was corrected before the
passing qualification, and its failed run remains in the archive.

The exact source was built with Rust 1.97.1 and installed on the Air. Binary
SHA-256 is `e2c906fe955ba3147c8dd9bb64ec7bbb6dc956db6a60d35438385628e66000ef`;
served alignment-owner SHA-256 is
`785140c1c3b5cbaa4aeb2a3506361c9682d4c97eabb0c2f5ad29b2b33298cbba`.
The existing network-volume grant was renewed for the updated binary.

At 21:17:07 UTC, HDR remux incumbent
`91d727cc-86e9-48fd-864e-6ff5fb21f423` commits SDR successor
`e123d10f-2295-4d1f-b412-147cb297a53d`. The diagnostic changes only the
successor capability request to SDR after HDR playback starts; observation
wrappers return the production functions' unmodified results. Pinned FFprobe
confirms H.264 1920×1080 BT.709 with AAC in the attached rendition
`6a3c64ce9f08e64e854fee57f01e62a770948d672bde51ff425d2bcb8844e898`.
This is an applied SDR transition, unlike the earlier HDR-preserving pass.

The decoded guard rejects a 41.959 ms pair, then accepts a 41.25 ms pair
within the measured 41.709 ms cadence at a common display instant. The
original tolerance, freshness, 800 ms deadline, bounded seeks and ownership
checks remain enforced. Preparation takes 3,180 ms; first-frame presentation
follows the internal commit timestamp by 31.5 ms. Tap-to-first-frame is
3,766.7 ms, including preparation. At film time 224.401 s, the successor is
still playing at 1920×1080, quality 1080, unmuted with volume 1; 5,290 decoded
frames and zero recorded postcommit hitches/faults are retained. Those
postcommit counters do not establish zero frame loss at the switch boundary.
The switch frame ledger and physical speaker seam remain unmeasured; control
measurement uploads return 429/425. The 21:14 warm trial safely refuses under
9.8 s incumbent runway before alignment and is preserved separately.

The [41-member final source, qualification and physical archive](http://forge.lan:3000/attachments/26063af1-42b0-4c76-84b7-49769b9cb135)
is 64,419,028 bytes with SHA-256
`cbbbcb741c83b9721fa2f1b1c1fb477e1c35dcdc39e2f31907f75fdf8fb97642`.
It includes the exact committed source archive, build/install identities,
complete frame/state logs, real output probe, passing receipts, landing,
retained pressure refusal and restoration. The preceding
[22-member review and qualification controls](http://forge.lan:3000/attachments/d3c2e45e-bb9e-4d96-8c9e-c0f2a3e6e145)
have SHA-256 `36fc1931321f4021d7cc52e52115e8656a89db6896b069482508bfaad09c27c8`.
All original Oct 12 raw log lengths and hashes were reverified; earlier
failures and archives remain intact.

At 21:25:41 UTC, a fresh authenticated item read verifies resume
`position_ms: 0`, `watched: false`, quality Auto and no active browser session.
Reload removes temporary capabilities and observation wrappers; the movie
page shows Play. Shared clipboard is OFF, and temporary keyboard sync is
reverted. Backend Auto and Mac processing ON were unchanged. The bounded
ordinary Air hls.js F2 alignment case is complete. Physical speaker continuity,
the broader continuous-quality platform matrix and F3–F7 remain separate work.

## 5. F3 — abandoned preparation occupies VOD capacity

### Observation and evidence

After failed preparation and browser relaunch/profile changes, the test's own
session remained in Activity, client unobserved, producer held ahead, with GPU
capacity shown as 1/2. Two new starts failed at 22:03:31.885455 and
22:04:35.815610, 18:03 and 18:04 EDT:

```text
vod_family_capacity: the continuous media group cannot be admitted together
```

Both were surfaced as HTTP 500/internal errors. Stopping only that test's
session via Activity cleared the operational obstruction. The subsequent
corrected Metal start at 22:06:24.910103 took 2.363 seconds and completed the
five-minute uninterrupted run. See `air-94-metal-startup-error.log`,
`air-94-metal-retry.log`, `air-94-capacity-error.jpg` and
`air-94-metal-5m-proof.log`.

This establishes that retiring the owned session allowed admission. It does
not yet establish a memory/resource leak, the intended retention lifetime,
or whether the retained reservation was still legitimately owned.

### Investigation

1. Inventory the failed preparation's session, family, reservations, pins,
   producer, viewer lease and terminal state before and after abandonment.
   Trace acquisition and release to their existing owners.
2. Distinguish explicit cancel, failed change, tab close, browser quit and
   missing heartbeat. Identify the intended TTL/grace for each and whether
   restart reconciliation applies. A disconnected browser is not by itself
   proof that all server resources should be immediately destroyed.
3. Compare actual reservation accounting with the compound family admission
   decision. Determine which retained charge caused refusal, including any
   shared producer or another valid viewer.
4. Reconcile with [scratch reservation work](../streaming/SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md)
   before building another ledger. Separately inspect whether expected
   capacity pressure should be classified differently from an internal 500.

### Acceptance and regression

Use owned sessions to cover failed preparation, cancel, supersession, browser
exit, lease expiry and restart reconciliation. Include same-family reuse,
another family and two legitimate viewers. Abandoned charges and pins return
to the intended baseline within the declared policy; active viewers retain
their resources. Admission after cleanup succeeds without Activity manually
stopping an abandoned test session.

Normal pressure must still refuse safely and explain the retry condition.
The repair must preserve compound admission and resource limits. Prove the
terminal release in a regression at the owning state transition and record
before/after accounting in the physical receipt. Never stop another viewer
to make the capacity test pass.

### October 15 current owner controls

Twelve focused current-source regressions pass for exact cancellation,
late cancellation, supersession, idle expiry, restart cleanup, compound
capacity refusal, stopped-producer permit transfer and active-reader
protection. Their exact commands and individual results are retained in the
[controlled receipt](evidence/f6-authority-media-20261015.json). The idle
producer fixture uses the real driver and admission ledger with a finite
sleep child. The later [real lifecycle receipt](evidence/f3-real-lifecycle-20261015.json)
adds actual owned media, admission accounting, scheduled pins, idle expiry,
restart and ordinary rolling browser teardown. The original three-minute
retention observation alone does not prove an ownership leak; historical
attribution remains inconclusive.

## 6. F4 — laptop cold/remux Heretic startup remains unreliable

### Observation and evidence

Earlier Heretic starts encountered placement/startup failures, stalls and
quality-unavailable/refusal outcomes. A warmed forced-1080p SDR attempt
ultimately played: browser intrinsic size 2583×1080, 2,268 frames, zero drops
and zero interruptions. The paused position was 94.358 seconds into the
movie. **94.358 seconds is not startup time.** Intrinsic display width also
is not proof of the encoder's stored raster without sample-aspect facts.

Retained records include `laptop-heretic-20261012-failure.txt`,
`laptop-heretic-20261012-session.log`, `laptop-heretic-final-failure.txt`,
`laptop-heretic-quality-unavailable.txt`, `laptop-heretic-warm-retry.txt`,
`laptop-heretic-source-read.txt`, `laptop-heretic-diagnostics.txt` and
`laptop-heretic-1080-started-20261012.txt`/`.jpg`.

The parser-install/identity failure is repaired, and source access was checked
separately. The surviving cold/remux symptoms need their own diagnosis; the
warmed success does not identify the earlier bottleneck.

### Investigation

1. Hold source, stream, node/build, client/profile, requested quality and
   preferences constant. Record exactly which state is cold: catalog probe,
   fragment index, rendition artifacts, producer or client attachment. A fresh
   browser tab with existing artifacts is not a cold server start.
2. For matched cold/warm trials, measure source open, probe, catalog lookup,
   index readiness, placement/admission, producer start, first published
   media, HTTP delivery, client append and first presented frame separately.
   Associate every stage with one request/attempt and owner epoch.
3. Separate remux, forced SDR transcode and quality refusal outcomes. Verify
   actual selected-stream HDR/DV facts and supported route before comparing
   with the Air's ordinary HDR10 case.
4. Establish whether time is spent reading, computing, queued for capacity,
   waiting for authority, or waiting for the client. Retry and independent
   file-descriptor diagnostics are evidence to interpret, not a root cause.

### Acceptance and regression

Declare startup budget and repeated cold/warm trial count before testing.
Use isolated artifacts or an owned test namespace to establish cold state;
do not erase a shared cache. Every trial ends in a proved first frame within
the budget or a truthful bounded refusal with an attributed stage. A warmed
retry cannot replace a failing cold row.

Add the smallest regression for the isolated bottleneck or incorrect deadline,
then repeat the original cold route and forced-SDR control. Publish latency
per stage, failure rate, route and source identity. Keep unsupported-source
refusal separate from supported playback reliability.

### Compressed Matroska startup repair — 2026-10-15

The equivalent AV1 HDR10 control completed three original-quality cold/warm
pairs within the predeclared 40-second startup budget. Its first forced-1080p
SDR cold start instead refused before producer admission with
`vod_source_rescan_required`. The retained native and sealed probe documents
isolated the difference to four PGS subtitle streams: native FFprobe reported
1920×1080 geometry, while the sealed parser omitted both dimensions and logged
`Unsupported encoding type`. The movie bytes had not changed.

The sealed parser disabled dependency autodetection without supplying zlib.
The repair builds the same pinned zlib commit as the native package for WASI,
requires compiled zlib support, and retains its authenticated source and license.
It preserves the source-comparison guard, parser ABI and capabilities. Relative
include/library paths avoid embedding temporary build directories in the module.

The [parser receipt](evidence/f4-compressed-matroska-parser-20261015.json)
records the final module digest and actual held-file proof: all nineteen complete
stream dictionaries equal native FFprobe, including the four PGS geometries,
with empty stderr. Thirteen package regressions pass, including missing compiled
support, substituted-source refusal before compilation, and recursive source/license
retention. Astra approved the correction. The later [physical playback receipt](evidence/hdr-physical-playback-20261015.json)
records the completed forced-SDR startup matrix and bounded Air captures.
This parser receipt by itself proves source-fact parity, not playback.

## 7. F5 — playback information does not fully describe delivery

### Observation and evidence

Cached VOD PlaybackInfo lacked renderer/pipeline and encoded-frame rows;
“Stream frame” was unavailable. Earlier laptop HDR badges disagreed with SDR
delivery. INFO-level VOD logs omitted FFmpeg argv. Acceptance therefore used
the actual running producer command plus browser intrinsic dimensions, not
the panel alone, to prove the Air graph and output.

A `vod` encoder/producer-held label describes delivery/production state; it
is not the actual tone-map renderer. See
`air-94-metal-5m-presented-info.jpg`,
`air-94-metal-producer-command.log`, `air-94-cpu-producer-command.log` and
`laptop-heretic-diagnostics.txt`.

### Investigation

1. Trace planned recipe, effective producer graph, published artifact facts
   and currently attached client rendition as separate evidence sources.
   Identify missing fields and where current code obtains each displayed row.
2. Bind actual facts to source identity, rendition/generation and cache
   provenance. A cached artifact can outlive a preference change or a producer;
   current node settings cannot reconstruct its actual renderer.
3. Distinguish encoded raster, sample/display aspect ratio and client intrinsic
   size. Likewise distinguish source HDR metadata, delivered transfer/color
   facts, negotiated capability and actual processing contribution.
4. Join diagnostics without exposing credentials, capability URLs or real
   infrastructure in mirrored receipts. Prefer structured provenance over
   inferring a graph from a template or changing global logging preferences.

### Acceptance and regression

Check fresh and cached artifacts, CPU/Metal, successful and failed quality
changes, stop/reopen and alternate-owner routing. UI/API facts must follow
the attached delivered rendition. After F2's refusal they still describe the
incumbent HDR remux; after a successful SDR change they describe SDR.

Show unknown when actual evidence is absent. Add focused identity/provenance
and presentation regressions at the discovered seam, including stale replies
and artifact reuse across preference changes. An actual graph receipt and
client delivery facts must remain independently inspectable.

### Built repair — local checks passed, qualification pending

The F5 task reuses the existing optional `stream_format` wire contract for
VOD status. It reads codec, effective dimensions, encoder, renderer, grade
and tool build from the attached immutable encoding plan. Copy dimensions
are labelled as an unchanged video selection; source DV labels never stand
in for its delivered grade. No media probe or strict control-protocol change
is added. A saved Developer preference cannot relabel an older cached plan.

The web adapter binds the descriptor to the attached status/session, keeps
source, planned stream and browser intrinsic dimensions separate, and follows
quality success, refusal, reopen and stop. Planned SAR remains distinct from
a measurement. Encoder notes name the frozen renderer. The badge follows
the attached output grade and continues to state that display output is
unverified. SDR/copy sessions no longer report a fictitious HDR peak; explicit
GPU peaks use the same frozen-plan provenance as CPU peaks.

The missing server descriptor and hardcoded web `Unavailable` are reproduced
before the fix. Two focused server tests, 221 web-policy cases, full web
unit/static checks, Clippy and the input fence pass locally. The two browser
fixtures initially skipped for a missing Python dependency; isolated reruns
then prove both startup paths/local seek and subtitle-readiness presentation.
The [bounded receipt](evidence/f5-output-facts-20261015.json) pins the retained
failures and controls. Independent review, task gates and actual fresh/cached
CPU/Metal reporting acceptance are still pending. This is not completion of
the broader native/direct/rolling output-facts handoff.

## 8. F6 — serving authority briefly self-fences and startup returns 503

### Observation and evidence

During the earlier `4e` run around 20:46 UTC, 16:46 EDT, replicated state
application/quorum watermark operations timed out. A startup request returned
503 after approximately 4,216 ms while claiming its media-session request.
Authority recovered at 20:46:35.737490. The transcode recovery line reports:

```text
outage_ms=2468 outage_budget_spent_ms=2468 kept_sessions=0 grace_ms=5000
```

This proves a 2.468-second authority outage and recovery within that logged
grace. `kept_sessions=0` does not prove continuity for an active existing
viewer. Request latency, sampling outage and serving outage have different
anchors. See `air-4e-final-playback.log`.

### Investigation

1. Join required/applied watermark, term, owner epoch, transaction/apply times,
   authority loss/recovery and the specific rejected request. Determine the
   slow operation and compare with existing [quorum incident work](../streaming/HEATED-RIVALRY-S1E5-QUORUM-RCA-AND-FIX.md).
2. Separate admission fencing, an already-attached viewer's grace handling,
   owner migration and actual client frame interruption. Collect explicit
   active-session counts and frame evidence for any continuity claim.
3. Check recovery/retry classification and stale-owner rejection after authority
   returns. Preserve the authority guard while diagnosing its latency.

### Acceptance and regression

The recovered historical incident can be dispositioned with an attributed
cause and a current-source reproduction or explicit “not reproduced” result.
A controlled authority-delay case must preserve stale-owner fencing, recover
within its declared policy and give startup a truthful retryable result.
Existing-viewer continuity needs a nonzero owned session and independent
frame evidence. Exceeding grace needs a separate bounded terminal outcome.

Use an isolated fault-injection environment for deterministic regression.
This document does not request a live cluster partition, voter removal or
restart campaign. Disabling the guard to hide 503s does not qualify a repair.

### October 15 controlled media qualification

The [controlled receipt](evidence/f6-authority-media-20261015.json) preserves
an actual realtime H.264 HLS publisher, registered with the existing session
manager, and an independent frame decoder. A 2,468 ms authority loss keeps
the same child and registered session. Completed media advances beyond the
highest segment observed at the loss boundary, and again beyond the recovery
boundary; independent decoded pictures differ across those observations.
New admission stays fenced during the loss, fresh admission recovers, and the
old generation remains invalid. After stable recovery, a separate sustained
loss retires the still-live producer and completes child cleanup.

The original test used a two-second accelerated grace and correctly failed
this positive control. Tests now share production's unchanged five-second
constant. The actual-media control and 25 affected fence regressions pass;
the initial failure and review's repaired sampling-boundary gap remain
retained. This is source/media continuity, not a browser frame-gap measure.
No live cluster fault was injected. The old replicated-operation delay is not
reproduced here and cannot be attributed from the retained incident excerpt;
no unrelated quorum incident is substituted as its cause.

## 9. F7 — residual grade difference and weak synthetic parity evidence

### Observation and evidence

The corrected peak serialization removes the old excess brightness and
highlight clipping, but Metal remains slightly darker than CPU in the real
five-minute comparison. Raw decoded-frame diagnostics report:

| Measurement at the matched 300 s frame | Old Metal | Corrected Metal |
|---|---|---|
| Mean luma | 57.7974 | 38.9083 |
| Luma mean absolute error against CPU | 13.7472 | 5.1532 |
| 95th-percentile absolute luma error | 23 | 9 |
| Maximum absolute luma error | 102 | 31 |

CPU mean luma was 44.0563. These are code-value diagnostics on a matched raw
frame, not display calibration or a perceptual grade score. See
`plurx-air-real-frame-check.log`/`.py` and
`plurx-metal-peak-check.log`/`.py`.

For this pinned FFmpeg build, Metal peak uses nits/10: 1179 becomes 117.9.
The CPU/CUDA/OpenCL terms use nits/100: 11.79. HLG is unchanged. A 1000-nit
fallback can mask the former serialization error; a non-round 1179-nit
neutral PQ diagnostic exposes it. The broad colored `testsrc2` boot probe
allows mean-channel difference 24 and accepts both variants: the diagnostic
record reports 8.209 for old and 8.341 for corrected. Passing that fixture
cannot establish detection of this real-source regression. See
`plurx-metal-boot-fixture-check.log`/`.py`.

### Investigation

1. Preserve absolute-unit and affected-cache-key regressions already landed.
   Build a deterministic diagnostic that fails the old serialization and
   passes the correction, with neutral PQ signals and non-round peaks.
2. Separate peak-unit regression detection from grade fidelity. Compare shadows,
   skin tones, neutral regions, saturated colors and unclipped highlights
   across multiple genuine HDR10 sources and timestamps, with matched frames,
   range, dimensions, transfer and tool identities.
3. Investigate Hable implementation, EOTF/gamma, gamut/range conversion and
   clipping before attributing residual darkness to one parameter. Keep HLG
   and Dolby Vision qualification in their own reference domains.
4. Declare the reference and thresholds independently before measuring. Follow
   [HDR reference scoring](../performance/HDR-REFERENCE-SCORING.md): its PQ
   pair scorer refuses SDR/HLG/DV inputs, while SDR VMAF requires a separate
   pinned SDR grade receipt. Its 1000-nit reference assumption does not by
   itself settle this sample's 1179-nit grade.

### Acceptance and regression

Close the narrow probe follow-up when the declared diagnostic demonstrably
rejects the old peak graph without rejecting qualified corrected controls.
Close broader fidelity only with a named corpus, independently pinned grade,
per-frame metrics, preserved failures and physical display evidence. Define
which differences are tolerable before seeing the final measurements.

Screen Sharing captures and forced-sRGB browser profiles are useful relative
observations, not calibrated physical-display measurements. Do not tune the
production curve to match one film frame or report SDR VMAF as HDR VMAF.
A parity-probe improvement and a calibrated grade qualification are separate
closure receipts.

### Narrow neutral guard — built locally, qualification pending

The F7 task adds a separate 512×128, eight-frame neutral PQ correctness
check for Metal, with MaxCLL 1179 and lossless 10-bit HEVC input. It keeps
the existing 4K colored-motion speed measurement and the unchanged 24-level
channel tolerance. The corrected production curve and its peak units are
unchanged. A failure to generate, encode, tag or measure the neutral control
refuses Metal parity with its specific reason.

The actual Mac control reproduces the former blind spot: old units pass the
motion image check at 8.2315 levels but fail the new neutral gate at 26.35.
Corrected units pass both; the eight-frame encoded neutral diagnostic differs
from CPU by 10.24 levels. The focused regression fails before the repair;
48 ordinary probe tests and the explicit real-Metal negative/control test
pass locally. The first hardware test incorrectly required Matroska container
transfer/primaries, which were omitted; decoded frames prove PQ, BT.2020,
10-bit geometry and MaxCLL. That failed assertion and the corrected run are
preserved together. These controls do not close broader film/display fidelity.

The [bounded receipt](evidence/f7-neutral-peak-20261015.json) pins the source,
tools, commands, old/corrected measurements and retained archive hash.
Astra approved the narrow guard and its seven-job effort gate passed; PR1101
landed in the effort as `3c757c5e8`. Final main qualification and broader
physical display acceptance remain open.

### Hable reference white — bounded repair, shipping acceptance pending

The remaining darkness has a separate identified cause: the accepted CPU chain
linearizes PQ relative to 100 nits, while pinned Jellyfin Metal uses 203 nits.
Matching the explicit peak unit does not make nonlinear Hable identical.
A separate native patch adds a bounded `reference_white` option, default 203,
and normalizes both signal and peak inside Hable. Plurx requests 100 for its
PQ Metal graph. CPU, CUDA/OpenCL and Metal HLG BT.2390/ITP remain unchanged;
Metal peak serialization remains nits/10. Old packages without the required
option report the graph unavailable through the existing parity/resolver path.

Before running the corrected variant, a separate neutral limit of two levels
was declared. The broad motion limit remains 24 and its encode clock remains
the throughput measurement. Actual eight-frame VT-encoded neutral controls
measure a maximum mean-channel delta of 10.2490 for old 203-nit Metal and
0.3227 for corrected 100-nit Metal; all outputs have verified BT.709 tags.
The raw neutral luma MAE falls from 10.2295 to 0.3561. Nine held real-film
frames all improve, with pooled luma MAE falling from 5.07994 to 0.19135.
Default 203-nit output is byte-identical, invalid bounds refuse, and authored
HLG BT.2390 controls are byte-identical with and without the new option.

Astra independently approved the bounded native prototype and verified its
source and retained bytes. That review is not production-integration approval.
The [reference-white receipt](evidence/f7-reference-white-20261015.json) links
all controls and the independently downloaded private archive. The limited
proof executable was never installed. Full shipping-package/source/parser
qualification, integrated regression/review gates and physical Air acceptance
remain required. Available Apple factory reference presets need no new
calibrator purchase; independent instrumented calibration and an artistic SDR
grade remain unclaimed. Individual pixel differences persist despite low
aggregate error, so these controls do not close general film/display fidelity.

## 10. Shared execution and closure contract

### 10.1 Minimum receipt for each reproduction

Record these fields in the existing owning ledger, with linked raw evidence:

| Group | Required facts |
|---|---|
| Identity | Main/patch commit, installed build, tool hashes, node role/epoch, browser/client version and profile |
| Source | Stable source identity, selected stream and HDR/DV/color facts, requested and delivered recipe |
| State | Explicit cold/warm definition, active/prepared generation, owned sessions, buffer/readiness ranges, capacity before/after |
| Operation | Attempt/control sequence, accepted target, timestamp anchors, supersession/cancellation and terminal result |
| Measurements | Startup/seek/switch stage durations, actual presented frame, dropped/interruption counts with their observation window |
| Outcome | Passed/failed/not run/not reproduced; confirmed mechanism versus remaining hypothesis; restoration and retained artifacts |

A useful failure taxonomy keeps source access, admission pressure, authority
fencing, preparation alignment, client delivery and presentation separate.
HTTP status or a final generic timeout alone is not a causal diagnosis.

### 10.2 What closes a package

Each package needs a current-baseline reproduction or documented inability to
reproduce, a justified disposition, a bounded repair where needed, the
smallest meaningful regression for that repair, and the physical acceptance
rows its contract requires. Source tests cannot replace frame/audio/device
proof. Physical success cannot replace a missing cancellation or accounting
regression. Record all failed attempts, even when a later retry succeeds.

If no defect is found, record the intended policy and the evidence showing
that it behaved correctly; identify any remaining UI or acceptance gap.
Do not silently close a historical observation because main moved.

Future behavior changes follow the repository's normal compiler, regression,
review and promotion workflow. This document itself is documentation only;
no playback rerun, code change, hardware action or new qualification was
performed to publish it. Update [the backlog](../features/PROJECT-BACKLOG.md),
the owning canonical ledger and this document together as F1–F7 progress.
The accepted HDR10 slice remains closed while those broader packages are open.

## October 15 physical playback and restoration

The [physical receipt](evidence/hdr-physical-playback-20261015.json) binds
the complete native package, corrected sealed parser, actual reporting UI
and cold/warm route controls to their immutable private attachments. The
original failures remain in those archives. These are isolated backend
acceptance namespaces; no branch mobile application was installed.

Three genuine AV1 HDR10 original-quality cold/warm pairs presented within
the declared 40-second budget. Three corrected manual-1080 SDR pairs also
presented: cold 3,336/4,264/4,246 ms, warm 761/1,380/817 ms. Every sampled
SDR run reported zero buffering interruptions. Frame drops, background
indexing and the older original-quality runtime are disclosed in the receipt.
The initial compressed-PGS source-rescan refusal is retained alongside the
parser correction; the held-source guard was never relaxed.

On the Air, the complete corrected native package admitted Metal at 2.285×
the CPU boot fixture. The System page shows Metal and explains that Vulkan
is unavailable. Actual full-film 1920×1080 SDR Metal playback reached 5:28
with 7,854 frames, five drops and zero interruptions; TTFF was 2,473 ms.
The CPU control reached 6:41 with 9,596 frames and 30 drops, and retained
one early 1,019 ms supply stall; TTFF was 12,497 ms. Exact child graphs
prove Metal reference-white 100 and CPU reference-white 100. The initial
Auto/remux failures are retained and are not attributed to either forced
SDR graph. Screen Sharing carries SDR, so this proves neither an optical
HDR grade nor independent physical audio.

Fresh and cached CPU/Metal reporting controls preserve attached-output
facts across live settings changes and candidate refusal. Their separate
archive records that the 60-second media control repeats a 12-second film
excerpt. It is a reporting check, not another independent film test.

The final Air API read verifies zero sessions/deliveries, zero hardware and
software occupancy, Mac processing on, and both movie positions zero and
unwatched. The owned Air server is stopped, shared clipboard is off and
the temporary Metal SDK is removed. The working NAS mount is retained.
The later F3 receipt adds bounded current-owner accounting. Final current-tree
qualification/promotion remains tracked work; this receipt does not turn a
pending gate into a pass.

## October 15 lifecycle finding and finite closeout scope

The [real lifecycle receipt](evidence/f3-real-lifecycle-20261015.json) closes
F3's observed obstruction as **current-owner controls pass; historical
attribution inconclusive**. It does not invent a new leak repair. The private
archive retains all original driver failures and 113 manifest members; its
downloaded bytes match the recorded digest. Astra independently checked them.

A genuine AV1 HDR10 source creates a two-video SDR family with shared AAC.
A second viewer reuses the same family without increasing the observed
hardware/software charge. A different family receives retryable HTTP 503
`transcode_capacity_pending`. Releasing one viewer preserves the other's
actual media. Ready cancellation and supersession clear the old ready facts.
Two scheduled video intervals reserve 940,101 bytes; cancellation keeps the
conservative reservation until exact disposal facts release it to zero.
The serialized terminal ledger retains 82,852 bytes of historical shared-audio
interval rows; those rows are not asserted to disappear.

The abandoned reader attaches at 19:35:04.384 UTC and idle-reaps at
19:40:09.965, about 306 seconds later: the 300-second sliding TTL plus its
maintenance tick. By the campaign's 300-second observation it is ended while
the other reader's 20-second hold heartbeats continue successfully. Final
client release returns sessions, deliveries and hardware/software occupancy
to zero, and another family admits without Activity Stop. An exact owned
server restart reconciles to zero occupancy and admits a fresh family.
Ordinary Chrome rolling AV1 playback advances beyond two minutes; paused
seek-to-zero, exact tab close and final API reads verify ordinary browser
teardown and zero/unwatched restoration. The owned backend is then stopped.

This is a finite current-owner result. A decoder/preparation failure after
allocation, abrupt browser-process quit and controlled-family prepared-change
browser teardown are not separately isolated here. The original three-minute
retention was inside the documented idle interval and does not establish a
leak. These limits remain in the broader A04 matrix; they do not erase the
measured current-owner result.

| Follow-up | Bounded result retained for promotion | Separate limits |
|---|---|---|
| F1 | Existing main repairs and ordinary Air paused seek/reopen acceptance complete. | Other devices/native HLS remain in their platform matrix. |
| F2 | Existing main repairs and decoded Air SDR transition acceptance complete. | Independent physical audio and broader transition matrix remain separate. |
| F3 | Current owner controls pass; natural expiry, cancellation, supersession, reservations, restart and ordinary rolling browser teardown recorded. | Historical attribution inconclusive; post-allocation failure and abrupt quit not isolated. |
| F4 | Sealed compressed-Matroska parser repaired; three genuine original-quality and three corrected SDR cold/warm pairs present within the declared budget. | Original-quality pairs predate parser repair; drops/background indexing disclosed. |
| F5 | Attached-output facts remain stable across CPU/Metal settings changes, cached reuse and refused candidates. | Reporting media repeats a 12-second source excerpt. |
| F6 | Real producer preserves distinct encoded media through a 2.47-second authority loss; sustained loss reaps after the shipping grace. | Original lag attribution is not retrospectively proved. |
| F7 | Neutral guard, corrected Hable reference white, held-film/HLG/default controls, full native package and five-minute Air Metal/CPU comparison recorded. | CPU has one early supply stall; initial Auto/remux failures, optical calibration and independent audio are not hidden. |

Task qualification records are exact-head receipts. The final frozen effort
still requires its full main promotion gate and qualification receipt. Windows
compilation is absent by the maintainer's October 11 pipeline amendment; no
Windows success is claimed. Production deployment and the broader codec,
caption, optical and device matrices are not inferred from these records.

## October 15 inherited Android navigation prerequisite

Promotion run 5228's Android instrumented suite completes 78 tests with one
failure: the shared transport-order control expects Right from Playback
settings to reach Playback info, but the new Report a problem button
intercepts it. An isolated owned Android TV emulator reproduces the same
failure in the eight-test focus class. Moving reporting after the contiguous
standard options restores Settings → Info → Picture in picture. The same
control now also proves Right reaches reporting afterward and Left returns
to Picture in picture; no focus assertion is removed. All eight focus tests
then pass on the owned API36 emulator. Failed baseline/build/test output is
retained, and no branch app is installed on a physical device. Independent
review, task qualification and fresh full promotion remain required.
## October 15 inherited Apple qualification prerequisite

Full promotion run 5228 passes policy/preflight and the fast Rust compiler,
but its Apple suite exposes four inherited failing cases in files unchanged
by the HDR effort. An isolated iOS simulator reproduces all four (five
assertion failures): the Developer inventory omits the existing Remote
diagnostics advisory; two subtitle controls dequeue a processing-status
notification instead of their subtitle callback; and the terminal-publication
control blocks the first shared callback before its pending warm-up can reach
the requested second exchange. The failed CI and local logs remain retained.
The failed local wrapper is stopped only after all assertions finish, while
Xcode is collecting simulator diagnostics; it is not recorded as a passing run.

The bounded test repair includes diagnostics in the placement inventory and
accepts its existing explicit "leaves Developer" graduation wording. Subtitle
controls hold all callbacks until a later request proves the ready response
was fully consumed, then execute them across the original owner/intent fence.
The terminal control starts with a terminal response, blocks exactly one
callback and joins reporter finalization before asserting no late verdict.
Production callback scheduling and playback fences are unchanged. Both entire
affected classes pass locally: 137 tests, zero failures, on Xcode 27.0 and an
owned iOS 26.5 simulator. No branch app is installed on a physical device.
Independent review, the task gate and fresh full promotion remain required.

Apple task gate 5234 reaches its ten-minute preflight deadline during the
full operations sweep, after all 359 validation tests pass. Its partial
operations output is not a completed suite or retained success. Preserve
the failed log and raise only the effort preflight to the existing fast
lane's finite 15-minute receipt-publication budget. Every history, policy,
validation and operations check remains required; the full main preflight
keeps its existing budget. The exact-head task gate must run again.
## October 15 inherited qualification prerequisites

Full promotion run 5228 preserves failures in the existing validation fixtures
and inventories as well as the Apple and Android controls recorded separately.
The prerequisite port takes the reviewed census, canonical cluster identity,
production storage layout, one-voter readiness, asynchronous snapshot and
real HLS browser fixtures from `d326ed2d648096b932b1e1bffc048b20e8d514f0`.
It retains the production consistent-read ceiling of 237, exact transaction
inventories, failure controls and original Makefile commands. The topology
performs two local queries per bounded operation, so its 256 operations require
exactly 512 local queries and zero Authority queries.

The reviewed Linux UI structure comes from
`d0433b54b98312996045913acbe0b7e12d546ff7`, SHA-256
`5fcd0589e40ee70ca25240345256b63e239442f5d9a653d79464c2df17de3419`.
The fixture waits for the current boot's caption probe and a stable empty child
inventory before captures, reserves four distinct listeners and supplies real
HLS structure. The earlier macOS capture's readiness and first-activity
differences remain preserved; importing the Linux golden does not claim a new
matching capture.

Pinned Rust 1.97.1 workspace/all-target checking and all 84 operation contracts
pass on the port. Focused consistent-read census, placeholder census, named
transaction inventory, topology schema and actual one-voter bootstrap/reopen
checks pass. All 70 macOS transport controls pass with no failures or ignored
cases, including both new delayed-snapshot controls. The attempted Linux count
wrapper correctly refuses 70 against 72; that failure is retained. Two cleanup
controls are Linux-only, so the shipping Linux requirement remains exactly 72.
Run 5228 also completes all 13 WAL snapshot-metric tests successfully before
its stale count of ten refuses them; the guard now requires all 13. Daemon
activity cases execute serially against the actual production storage layout.

Astra approves the frozen 17-file source delta, SHA-256
`70fad91d01c1f78b7823e4fd57c04723fa2716b2b10d25a89662cd9a007bb22b`.
Its review leaves integration and exact-candidate task/full gates required.
All three failed legacy Store controls reproduce the same mixed-sharing-lineage
refusal locally. Their historical downgrade fixtures still contain later empty
sharing tables. The port adds the existing guarded fixture rewind: it requires
the actual baseline marker, no Source activation and zero rows in every
inventoried sharing table before removing those disposable tables. Each of the
three real controls then passes, including the populated three-voter fragment
prune and playback-mutation case. Production lineage refusal is unchanged.
The first local baseline compiler log was reused accidentally during rerun; its
exact panic/result is retained as a labelled terminal excerpt. Both other full
local baseline logs, all baseline result metadata and the original CI log are
preserved. The full Rust/Store job deadlines still require a new complete run.

The repaired browser fixture subsequently captures all 78 views with no page
or console errors; all 10,597 structural facts match the reviewed golden on
macOS as well. This uses the unchanged web/backend source surface from the
previous local build and the corrected fixture, rather than claiming a new
exact-candidate package build. Astra's Store caller review requests the four
companion baseline assertions for offline-audio and read-index replay. Those
exact assertions are ported without relaxing any guard, and both complete
replay callers pass locally. The initial 17-file approval and this addressed
caller finding remain separately identified.

The legacy Store job in run 5228 completes only 188 of 367 serial controls
before its 40-minute deadline, including three now-reproduced fixture failures.
Its measured throughput projects roughly 75 minutes including compilation.
The complete job therefore receives a finite 95-minute budget; no case,
assertion, failure status or required receipt is removed. The full Rust job has a separate incomplete-suite deadline, recorded below.

The complete local core run also exposes a macOS-only stale cached-subtitle
expectation: the QSV graph correctly includes the explicit Fontconfig provider,
while the test expects the platform-neutral string. Its repaired control checks
the exact `-vf` value with the required platform provider, retains the single
hardware download/upload and no-zscale assertions, and changes no production
graph. The original full-suite failure and isolated panic are preserved.
Run 5228's full Rust job likewise exhausts 30 minutes after compilation and
1,917 unit-result rows, before the unit/SQLite receipt; its finite budget is
60 minutes, retaining the entire suite and all required receipts.

The complete core suite on the prerequisite Rust snapshot finishes in
1,266.50 seconds: 1,880 pass, one cached-subtitle expectation fails and five
opt-in cases are ignored. All eight earlier core failures pass in that run.
The platform-corrected subtitle control then passes separately on the
current-main integration (one pass, zero failures). These are separate results,
not a claim of a fully green rerun. The new complete promotion remains required.

Rechecking the current-main integration finds two additional authoritative
managed-subtitle schema reads in `hiqlite.rs`: current shape decides whether
replicated DDL may run, and the installed marker must agree with that shape.
Both carry explicit authority reasons and the exact slice count becomes 48.
The overall unannotated ceiling stays 237 and both injected-read refusal
controls remain required. The initial integrated four-case census failure
(two pass, two fail) is preserved alongside the corrected rerun.

Task gate 5235 refuses the missing client-fix history anchor before compilation;
its failed receipt is preserved. The exact reviewed runtime commit and its
existing instrumented regression are now named in the client-fix catalog.
This catalog repair adds no runtime change or new device claim.

### Combined remaining qualification candidate

Task 5283 refuses a malformed client catalog after the Android/main merge
resolution loses one table separator. Reconstruct the catalog from qualified
effort `876162e2f` plus the exact independently reviewed Android row: every
existing entry is byte-for-value unchanged, all IDs remain unique, and the
repository loader and source/test anchors validate locally. The reviewed
Android source commits and original eight-test emulator proof join the
prerequisite candidate in #1137; #1129 is superseded only after that source
lands through the combined required gate. No failed gate is relabelled green.

Complete the existing reviewed historical and actual-Source fixture imports
from `d326ed2d648096b932b1e1bffc048b20e8d514f0`; upstream commits
`5f4c82f38` and `9887e4d19` record their Astra review. Preserve current main's
managed-subtitle tests and the corrected HDR reference-white assertion.
The current-main rerun passes four of five historical controls; read-index
replay correctly refuses the partial managed-subtitle extension left after
its historical queue rewind. Its fixture now requires baseline/no Source,
the current marker and complete six-object inventory, zero model receipts
and no managed accepted work before atomically restoring the published
predecessor resource view. Production schema guards and migrations stay
unchanged; the recorded failed run remains evidence for the corrected rerun.


The independent combined review requests two caller/oracle corrections: invoke
that managed rewind for both read-index opens, and capture the full trigger
name/SQL inventory before removing the three managed triggers. Both corrections
retain the exact restored-definition comparison; the revised source is approved.
The corrected complete read-index case passes locally in 10.13 seconds.

The complete v32 case then reaches its retained runtime reservation assertion and
exposes a separate production compatibility omission: the closed predecessor
inventory lacks the exact published v18/v31 session declarations after their
real additive migrations. Import the two pinned SQL variants and their existing
regression from `a06e00e10`, without admitting arbitrary layouts. The test keeps
historical row values and additive defaults, and refuses a substituted owner
constraint or extra index. Independent review verifies published provenance and
approves this narrow compatibility delta. Its focused execution and the fresh
exact-candidate gates remain separate requirements; the failed reservation log
is preserved.


The wider affected-case run preserves six additional failures: repeated
bootstrap replaces an installed managed resource view, two SQLite downgrade
fixtures retain the newer managed extension, and concurrent lineage migration
can reject a genuine custody-migration winner. All ten actual selected Source
runtime cases pass in that run. No complete-suite success is claimed.

Main #1140 lands while task gate 5293 starts, invalidating that gate's
main-to-effort ancestry. Its simpler non-destructive bootstrap creates the
probe-era view before the older resource installer and omits the migration-only
DROP. Adopt that exact main implementation: IF NOT EXISTS preserves an existing
managed view across retries and concurrent bootstraps, while final strict shape
verification remains required. The full retry scenario retains its model
receipt, accepted job, resource obligation and exact-view assertions. The earlier
reviewed guarded candidate and its two superseded helper tests remain preserved
in private evidence archive 106; they do not describe the final implementation.
The extra bootstrap shape read is removed, restoring that census slice to two;
the overall unannotated ceiling stays 237.
SQLite queue rewinds require current version 111, no Source installation,
exact managed objects/marker and no accepted managed work or receipts before
atomically restoring the published predecessor. Custody settlement reads its
marker, protocol and exact schema together: only absent predecessor 80 may
migrate, and only exact supported 81/82 may settle a concurrent winner. The
same-write predecessor and post-DDL shape assertions remain required. These
repairs require new independent review and focused qualification.


### Full current-base qualification retains additional fixture failures

Promotion 5306 rejects the unchanged Apple/Android build counters against
current main. Increase the source counters to Apple 252 and Android 172 for
the already integrated release-input changes; no branch app installation or
new physical acceptance is implied.

The parallel complete core run exposes three more historical-fixture failures.
The encoded-output and historical-ready-subtitle cases rewind below the managed
subtitle extension while leaving that extension installed, so reopen correctly
refuses duplicate schema creation. Both now use the existing exact-shape,
current-version, no-Source, empty-managed-work fixture rewind before changing
older objects or the version. The Dolby Vision migration inventory gains the
v111 entry whose actual removal both shared downgrade callers already perform.
The production migration and all data/history assertions remain unchanged.
Original isolated failures and the complete run retain their actual verdicts;
these corrections require independent review, focused runs and a fresh complete
promotion candidate.

Two further complete-core failures pin the old SQLite schema version 110 in
the sharing-upgrade and populated v6-rebuild controls. Explicitly advance both
pins to 111, naming the appended managed-subtitle migration, while retaining
all private-lineage refusal and preserved-library/data checks. The first Apple
build-reference suite also catches the Android README's old live counter; its
current claim moves to 172 while historical status entries remain intact.

Task gate 5308 refuses a missing client-fix history anchor for the combined
release-counter commit. Record that exact commit with the existing Apple and
Android current-target counter regressions; source anchors name the counter
fields, preserving the catalog across later monotonic bumps. This is catalog
bookkeeping, with no runtime source change. Preserve the refused gate and both
Python receipt artifacts, then rerun history locally before a fresh task gate.

### Complete promotion retains a stale Apple source-contract boundary

Promotion 5313's shared iPhone suite runs 1089 tests and retains one failure in
`testArmedBoundaryReplanEndsWithStallRecoveryAndSkipsBlockedCandidates`. The
current Auto evaluator accepts a `pressureOnly` parameter; the contract still
looks for its former zero-argument signature and fails before checking its
body. A local native run reproduces that same failure. Locate the function
without coupling the boundary to its parameters, retain every stall-recovery,
blocked-candidate, ownership and viewer-action assertion, and explicitly check
that pressure-only evaluations cannot spend the optional boundary re-plan.
Missing source boundaries now identify themselves in XCTest diagnostics.
Production playback source, build counters and historical physical receipts
are unchanged. The failing run stays failed; the reviewed test correction
requires its focused native run, task gate and a new exact-tree promotion.
