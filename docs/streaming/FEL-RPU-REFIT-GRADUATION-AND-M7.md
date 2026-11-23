# F+ graduation and NP1 trials — remaining acceptance

**Status:** open — isolated fit and served baseline acquisition complete;
the first frozen M7 candidate was rejected; physical graduation and runtime
comparison integration remain open. **Owner:** Codex. **Updated:** 2026-11-20.

Companion to [the build ledger](FEL-RPU-REFIT-BUILD.md): that records the
merged implementation; this records physical graduation and the next trial.
The [offline investigation](FEL-PICTURE-QUALITY-INVESTIGATION.md) explains why
NP1 is worth testing but is not a general replacement for F+.

## Boundary and retained evidence

Milestones 1–6 landed in #1565; documentation followed in #1796. No deployment
is authorized. The full lane passed on `f8a7f5ab3`; the final landing received
preflight only, under the recorded `Landed-Untested` exception. That historical
qualification gap remains open and is not covered by this acceptance work.
Reuse valid source-bound results; run only missing checks or failed checks.

The original nearest-Q12 chroma/Q23 luma native result remains 48/48, with
controls, on M3 Max/macOS 27.2.1. Its source, chart, RPU, renderer and device
identities remain unchanged in the [native receipt](FEL-RPU-REFIT-M6-Q12-2026-11-16.json).
It is not end-to-end plurx playback. The M3 is permanently retired. Paul moved
all former M3 work to the M5 Max MacBook Pro on 2026-11-19, superseding the
previous M5 restriction. New captures name the M5 and its actual OS; they do
not replace or relabel the original M3 measurements.

The retained isolated converter/VOD-publisher fixture read back 310 fitted
frames and a seek at 7.132 seconds. It is useful production-byte plumbing
proof, scoped to its recorded source; it is not a deployed HTTP session or
per-title four-model quality measurement. Historical Fp_safe scores predate
the native-qualified coefficient grid. Do not call them current served F+.

## Physical playback graduation

Current main places the F+ switches in Settings → Playback under its revised
Developer lifecycle policy. Their location does not establish copy-path or
physical acceptance. Retain separate evidence for every server-recognized
platform; ignoring curves or passing an offscreen render proves neither.
Mapping behavior is a measured fact, not a new client capability signal or a
decoder change.

| Platform | Retained evidence | Remaining physical evidence |
|---|---|---|
| Web | Safari fitted handoff, 60 ms seek and natural 206.864 s completion | Optical mapping and wider browser coverage |
| Apple | Physical tvOS fitted handoff and 65 ms seek; physical iPhone fitted handoff, 69 ms seek and natural 206.864 s completion | Optical mapping; native macOS coverage; unresolved tvOS cold-entry failure |
| Android | Physical Lenovo corrected fitted handoff, 911 ms seek and natural 206.864 s completion, zero interruptions and dropped frames | Optical mapping and wider device coverage |
| Linux player | Installed Player native P7 control and retained fitted P8.1 renderer control both reach natural EOF with zero drops | Bind same-run native FEL composition evidence; optical mapping |

Each run records the server commit/binary and processing bundle, client build,
physical device/OS/decoder/display, source content hash, session and rendition,
fit method and pinned curve digest, scene/window, fragment hashes and parsed
RPU curves. Capture start, seek into another scene, sustained playback and the
Playback Info result. Verify the converted copy path actually used the pinned
curves; a processed rendition, unfitted identity fallback or HDR10 fallback
cannot qualify fitted-copy playback. Record a separate unfitted identity case
with its reason. Keep settings and source media unchanged after the run.

Mapping observation is separate from successful playback. Use bound fitted
and identity controls and an independent premapped reference where the output
is observable. A protected/black capture is **unobservable**, not an A == B
pass or a failure to apply mapping. Decoder behavior must retain the measured
app/device scope. Do not prescribe all-platform numeric pixel equality from
the narrower M3 instrument, or interpret a software render as optical proof.

The M5 holds a hash-verified copy of the retained Michael source window and
new 48-frame metadata/BL/EL acquisitions. Their byte hashes, offsets and
metadata associations agree. The new production helper matched every base
and composed code over 6,036,480 fit and 6,036,480 held-out lattice samples
across all 48 frames, with zero position errors or mismatches. The retired M3 raw/NP1 archive was not found externally. These
new acquisitions are inputs, not replacement M6 or M7 quality passes.

The isolated native instance has now fitted its fixture through ordinary
jobs. A backend session with accepted typed Web control and a bound successor
served the fitted table; all 48 baseline frames read back against the source
clock and identical coded BL pictures. This acquisition is not a physical
playback pass. The in-app browser completed 310 frames without drops or
buffering interruptions as an HDR10 fallback control. Native Safari selected
Profile 8.1 but stalled after the video ended: the copy plan had advertised a
125 ms audio-only tail for a source with no audio. The correction derives
silent duration from indexed video and gives corrected silent plans a new
rendition identity, preserving real-audio tail behavior. Qualification of that
correction and actual fitted physical playback remain open.

A new rendition also exposed a shared-index mismatch: ordinary Analysis had
published a v3 index under the fitted recipe identity without the canonical
pre-conversion proofs and source-clock origin. The existing complete local
index retained both. Fitted resolution now requires those fields before
preferring a v3 candidate and otherwise retains the exact local/v2 fallback.
A future complete v3 remains eligible. Ordinary Analysis declines fitted
demand because its seal cannot produce that evidence; the dedicated fitted
index builder retains ownership. No proof requirement or artifact is removed.

The physical Apple TV identity-control run exposed a separate pinning gap.
The accepted control exchange already carries its typed platform, but normal
prepared quality handoffs dropped that context and reset the fitted digest.
The final reconciled copy candidate now resolves its pin from that exact
accepted platform; the durable successor preserves it for converting copy.
Stall-reopen fields remain absent. No client capability or wire field is
invented. An agent-operated physical tvOS run now committed the fitted
successor and continued to the end on the existing client. The client commit
requires a decoded successor frame at the boundary; source, daemon, fitted
digest and durable route transition are bound in the private receipt. An
earlier attempt was confounded by diagnostic media requests during staging;
the successful observation used metadata only. This scoped result does not
establish optical mapping, dropped frames, audio behavior or the remaining
platform coverage. Physical graduation remains open.

A later passive request trace on the same installed tvOS client bound a
206.864-second fixture to fitted digest
`87da3b1748ed2906ee51897d5fa31e19f5f2a91ec5fc9d49a0db45050a109065`.
The client committed its fitted successor and reported a presented-frame
seek resume in 65 ms. Its first cold fitted fragment took 3.141 seconds to
first byte and 4.399 seconds to complete. The complete media extent was
fetched. A final operator Back action may have exited playback, so this run
is not a natural-completion witness. The fixture repeats the original 310
coded pictures sixteen times; it supplies UI runway, not independent title
quality evidence. An earlier cold-entry timeout remains unresolved because
that attempt lacked per-request transport tracing. Successful playback does
not erase that failure or justify extending the handoff deadline.

The current host inventory sees a physical Apple TV 4K (3rd generation).
Its scoped playback result is recorded above. The inspected learner service
image is based on `254df39f7afb`, before the F+ merge; it cannot qualify this
feature. Paul authorized a fresh isolated M5 test instance on 2026-11-19, with its
own store and listeners and no production cluster join. No production service, client app or installed production processing bundle
is installed, restarted or upgraded as part of this work. Test fixtures must be identified
as fixtures and cannot alone satisfy real plurx session acceptance.

Native Safari 27.2 on the M5/macOS 27.2.1 also committed fitted copy,
performed a same-session buffered seek with a presented frame in 60 ms,
and naturally ended at 206.864 seconds. Playback Info reported fitted
curves, delivered Dolby Vision Profile 8, zero buffering interruptions and
five dropped of 1,399 total video frames at the terminal observation.
Display output was unverified. This is a scoped browser playback witness,
not optical mapping proof or native macOS/iOS client coverage. Its earlier
quality-change decline is retained separately from the successful successor.

The Lenovo client completed the identity control with zero dropped frames,
but its fitted handoff retained the predecessor. A separate diagnostic build
then observed the fitted successor initialize the Dolby decoder and render
its first frame, including after the alignment seek. The output metadata
still had no frame duration, so the existing one-frame alignment predicate
correctly refused it. The media-playlist entry had lost the HLS variant's
frame-rate declaration; increasing a decoder deadline would not repair that.

The correction measures the shortest positive presentation gap and declared
sample duration while the canonical fitted index is built. It sorts each
fragment's presentation timestamps, checks the boundary to the previous
fragment and withholds the optional bound for duplicate, overlapping or
invalid clocks. The summary travels with the immutable index, source clock
and picture/cadence proofs; fitted index keys advance independently of the
curve method so older measurements rebuild through ordinary indexing.
Android prepared scalar VOD uses a private durable single-variant envelope
with this attachment-owned bound. Missing evidence stays unknown. Apple
acquisition and subtitle behavior, the media child timeline, the strict
readiness predicate and handoff deadlines stay unchanged.

Further physical tracing identified two distinct causes. Ordinary fragment
landing compared a presentation timestamp against a decode timestamp. With
reordered frames, the retained fixture differed by 83 ms. The correction
compares the same presentation clock, retaining the strict uniqueness and
complete-window checks. Android then committed the fitted successor, but its
next seek opened an identity session: the active quality ledger still said
Auto. A proven successor frame now settles that ledger before queued seeks
are drained. Failed or rolled-back successors do not advance it.

The corrective Lenovo run retained the same fitted session and decoder
through one backward seek, resumed in 911 ms and naturally completed the
206.864-second fixture. Diagnostics reported zero buffering interruptions and
zero dropped of 4,444 frames. All 28 complete fitted segment bodies match the
immutable cache. The seek refetched one previously fetched segment; this is
an in-place session seek, not proof of memory-only delivery. The earlier
failed retention attempt remains recorded separately.

A wired iPhone 17 Pro Max running an isolated Cinema lab build also committed
fitted copy, resumed one buffered seek in 69 ms and naturally completed
206.864 seconds with zero reported buffering interruptions. Its final watch
state marked the complete extent watched; all 18 completely captured fitted
segment bodies match the cache. Neither this nor the Lenovo observation
establishes optical mapping.

The installed Linux Player's ordinary native P7 route completed 206.864
seconds, resumed a buffered seek in 65 ms and reported zero drops. A separate
physical renderer control played the exact retained fitted P8.1 init and
fragments to natural EOF at 130.290 seconds with zero drops; all 19 response
bodies match their retained hashes. Both controls exposed a PQ BT.2020 target.
The [Linux Player design](../clients/LINUX-PLAYER-PLAN.md#33-profile-7-fel-in-v1--composed-in-the-player)
intentionally claims native P7 because the patched player composes the full
enhancement layer locally. Its ordinary route therefore preserves the original
stream. The absence of a converted F+ successor does not establish a missing
Linux feature. Historical Player logs report enhancement-layer composition,
but that observation keeps its original run scope: this run did not capture
a composition flag. Its remaining native-path evidence is a same-run binding
of actual FEL composition. The compiled native-DV marker proves a build
capability, not per-frame composition. These controls qualify their recorded
paths and do not establish an in-app fitted F+ session. Device viewers and
test-owned endpoints were disconnected after collection.

## Native M5 test-instance prerequisites

CPU title scanning, F+ fitting and their fenced publication use portable tool
validation. Linux keeps GPU qualification and its index piggyback scanner.
The macOS scanner uses the ordinary source-work budget and leaves pending GPU
recipes and exact playback waiters for their capable worker. Held sources use
`/dev/fd/3` on macOS. Standalone and index-companion scanners share the
existing checked descriptor-manifest launch path and a daemon-owned cwd.
Native broker confinement remains an explicit unqualified recovery fact;
this work does not claim a clean recovery witness. No alternate fit algorithm
or decoder signal is added.

`tools/dv_processing/build-macos-bundle.py` builds from explicit static FFmpeg,
libdovi and libplacebo-387 prefixes. Supply `--provenance` with sources and
notices and repeat `--dependency-prefix` for transitive library metadata. It
relocates and signs the Mach-O dependency closure, then writes the same
source/ABI/hash-bound manifest with bundle-relative loading and no loader
environment override. Linux packaging retains its existing manifest behavior.
Point the isolated daemon's `PLURX_DV_PROCESSING_DIR` at that bundle; verify it
with `plurxd dv-helper-identity`. Keep HTTP, Raft and cluster API on distinct
loopback ports, a fresh data directory and no join token. Physical devices
may use an explicitly owned test proxy; no production service or client app
is replaced or upgraded.

## Milestone 7 sequence

1. **Bind the baseline.** For each title, obtain current-method fitted curves
   from the actual publication path. Retain whole and seek output/readback,
   source/scene/PTS binding and the exact helper master. Reuse an existing
   publication only when those bindings match; otherwise author an isolated
   fixture without changing a running service. Never substitute identity F,
   historical fine-grid Fp_safe, or a newly solved unserved curve for F+.
2. **Measure F+ first.** Evaluate those parsed-back curves over the same BL/EL
   inputs, frame grid, active region and reference-defined luminance bands in
   all four frozen composition/colour models. Keep full-frame and sampled
   results distinct. Preserve exact clocks and source transforms; no lag
   search or undocumented offset correction. Existing artifacts can be
   relocated with a new hash-bound location map; sealed requests stay intact.
3. **Freeze the protocol before NP1 scoring.** Record source windows, separate
   calibration/evaluation windows, frame coverage, rate envelope, recipe,
   four-model pooled and per-window measures, shadow-tail constraints and
   required improvement margin. Set the numeric margin from baseline
   calibration and representation/measurement repeatability before inspecting
   the new candidate scores. The earlier 0.02 bar was against Z6, not F+;
   copying it would invent an M7 acceptance criterion. Until this protocol
   exists, a comparison is exploratory and cannot approve a replacement.
4. **Run only the required NP1 trials.** NP1 retains psy, disables AQ strength,
   applies chroma QP offsets −9/−12 and uses the executed FEL-benefit map
   preserved as `lumazfsp` (the historical `lumazfs` naming discrepancy is
   recorded in the investigation). Bind the complete recipe and encoder
   hash, actual authored bitrate, metadata and frame clocks. Reuse existing
   NP1 bytes when every protocol/input/recipe binding matches; re-decode
   missing planes instead of re-encoding unchanged candidates. Do not silently
   replace NP1 with P1 or tune against held-out evaluation windows.
5. **Compare paired outputs.** Score NP1 and production-byte F+ against the
   same helper master in all four models, with identical frames/masks. Report
   per-window and pooled deltas, shadow tails, actual rate and the existing
   report-only grain proxy. A win in one model, a selected frame or against F
   is not a per-title M7 win. Keep inconclusive and losing trials: preserving
   the original copy path is a valid outcome.
6. **Integrate through title qualification.** Extend the existing
   `dv_fel_qualification` measurement path with the pinned F+ comparison arm;
   do not change the meaning of its existing served-versus-dropped verdict.
   Any reusable M7 result must bind source, window plan, F+ digest/method,
   helper/encoder/scorer identities, recipe, model and protocol versions.
   A changed binding invalidates only the affected result. Carry existing
   cancellation, admission and child-reaping ownership through the new arm;
   no second watchdog, decoder route or feature gate is needed.
7. **Keep replacement separate.** Only a full-title candidate that satisfies
   the frozen per-title margin and existing authoring/readback/timing and
   source-retention contracts could be promoted. This no-deployment task
   does not publish a full-title replacement or remove an original. The
   existing nonzero-video-origin disk refusal remains a separate open finding.

The historical calibrated native TopLeft encoder input was not recovered.
The new bounded CPU diagnostic export is a distinct representation with
explicit conversion, quantization, TopLeft sampling, source RPUs and clocks.
Independent validation matched all 48 frame identities and 24,883,200
full-plane samples in each calibration/evaluation export. Lossless NUT
packaging preserved every raw frame and the separate uniform encode clock.
It does not inherit historical native byte identity or quality wins.

The [Michael trial protocol](../../tools/dv_processing/diagnostics/fel_quality/protocol-m7-michael5400-20261119.json)
was frozen before candidate work: calibration frames 0–47 and 96–143,
evaluation frames 192–239, minimum 0.05 mean ΔITP gain in every model/window
and pooled result, and strict shadow-tail non-regression. The margin rounds
up the observed calibration model spread; it is a policy buffer, not a
statistical or perceptual significance claim. The authored 128.5–130 Mb/s
trial band is experimental and exceeds the original source rate; it gives
no delivery or replacement permission. Only the inherited NP1 recipe and at
most four rate-only attempts are allowed, within the fixed author envelope.

The actual served F+ evaluation baseline was measured once after protocol
freeze (Model B mean 1.397672 ΔITP). The other three frozen models were also
measured once. Baseline/none/zero same-input encoder controls passed, with
byte-identical elementary streams. They remain reusable while their consumed
inputs, encoder, harness and recipe bindings match. The bounded NP1 trial selected attempt 3 of 3. The current-core author and
independent metadata/clock/software-decode checks passed. Actual authored
video packets were 129.213487 Mb/s, within the frozen experimental band.

The [held-out result](FEL-RPU-REFIT-M7-2026-11-19.json) rejected NP1. All four
models worsened, and every prescribed shadow check failed. Model B mean
ΔITP was 1.503623 for NP1 versus 1.397672 for served F+; the required gain
was +0.05, while observed F+ minus NP1 was −0.105951. Its worst-frame
below-0.1-nit p99 rose from 3.685399 to 6.549889. The other models agree
on the direction. The scorer's exit 1 records a valid rejection, not an
execution failure. No fourth encode, tuning or scoring rerun is planned.
The source and F+ copy remain intact. This outcome applies only to the
frozen window and new representation; it does not settle historical NP1
results or generalize to a full title. Runtime title-qualification
integration is not implemented by this offline trial.

## Execution ledger

| Item | State | Next evidence |
|---|---|---|
| Original native M6 | Pass retained | No repeat of the 48-point M3 experiment |
| M5 migration | Source and exact helper/master binding retained | Preserve source/device identities |
| Physical matrix | tvOS fitted handoff and seek retained; earlier 52-second completion retained; full matrix open | Unresolved cold timeout, mapping, drops and remaining client/device scopes |
| M7 baseline | Two calibration windows and frozen evaluation192–239 bound and scored once in all four models | Retain source-bound scores |
| Numeric margin | Frozen at 0.05 with strict shadow-tail limits before candidate work | Apply unchanged to paired outputs |
| NP1 trials/integration | Three controls retained; frozen candidate rejected in four models; author/readback/rate admitted; integration open | Existing-job comparison integration and exact result reuse |

Adversarial review happens when the change is ready to merge. Afterwards run
only the remaining required fast-lane checks, preserving every valid pass.
Physical device availability and preflight success must never be promoted to
runtime or quality acceptance.
