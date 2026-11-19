# F+ graduation and NP1 trials — remaining acceptance

**Status:** open — isolated fit and served baseline acquisition complete;
physical graduation and M7 candidate trials remain open. **Owner:** Codex. **Updated:** 2026-11-19.

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

The Developer card requires copy-path serving and physical playback on each
server-recognized platform before its switches move to Settings → Playback.
Do not move the switches merely because a platform ignores the curves or an
offscreen renderer passes. Mapping behavior is a measured fact, not a new
client capability signal or a decoder change.

| Platform | Retained evidence | Remaining physical evidence |
|---|---|---|
| Web | No F+ plurx playback receipt | Actual browser/OS/display and served copy session |
| Apple | M3 offscreen mapping passes; M5 replacement host identified | Actual plurx client playback; record macOS/iOS/tvOS coverage separately |
| Android | Lenovo decoder mapping characterization in Google Photos | Actual plurx client playback; do not extrapolate that experiment to other devices |
| Linux player | No F+ physical receipt | Actual plurx Player, decoder and display path |

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
invented. Physical verification of this correction remains outstanding.

The current host inventory sees a physical Apple TV 4K (3rd generation).
That is availability, not playback evidence. The inspected learner service
image is based on `254df39f7afb`, before the F+ merge; it cannot qualify this
feature. Paul authorized a fresh isolated M5 test instance on 2026-11-19, with its
own store and listeners and no production cluster join. No service, client app or processing bundle is installed,
restarted or upgraded as part of this work. Test fixtures must be identified
as fixtures and cannot alone satisfy real plurx session acceptance.

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

## Execution ledger

| Item | State | Next evidence |
|---|---|---|
| Original native M6 | Pass retained | No repeat of the 48-point M3 experiment |
| M5 migration | Source recovered; exact helper/master binding passes all 48 frames | Served F+ baseline from isolated instance |
| Physical matrix | Open; isolated instance running; HDR10 control and native DV failure retained | Corrected native run and fitted client/device receipts |
| M7 baseline | Actual fitted-copy bytes and 48-frame readback acquired | Paired four-model F+ scores and separate evaluation/calibration coverage |
| Numeric margin | Awaiting baseline calibration | Protocol frozen before new NP1 candidate scores |
| NP1 trials/integration | Not yet run/built | Bound baseline/protocol; focused new-arm evidence |

Adversarial review happens when the change is ready to merge. Afterwards run
only the remaining required fast-lane checks, preserving every valid pass.
Physical device availability and preflight success must never be promoted to
runtime or quality acceptance.
