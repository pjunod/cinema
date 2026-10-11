# Dolby Vision timeline — bounded VFR, seek and epoch controls

**Status:** open — synthetic timeline replay reviewed;
production timing remains unqualified · **Updated:** 2026-10-08

Companion to the [implementation ledger](DV_HDR_PROCESSING_STATUS.md) and
[encoded-layer controls](DV_HDR_DECODED_LAYER_CONTROLS.md). This M0 follow-up
tests actual decoder timing through reordered variable-rate pictures, a real
demuxer seek, and two independently decodable inputs whose timestamps restart.
It retains separate evidence; the earlier approved bundles remain unchanged.

## 1. What is exercised

| Case | Executed control | Acceptance boundary |
|---|---|---|
| Variable frame rate | Six distinct BL/EL pictures with display PTS 0, 40, 110, 140, 230 and 300 ms, with and without B-frame reorder | Exact native pixels and fresh raw RPU identity; synthetic 64 × 64 pictures |
| Seek | Actual `avformat_seek_file` target 200 ms, followed by two BSF and two decoder flushes | Demuxer lands at 140 ms; decoded pictures are 140, 230 and 300 ms |
| Timestamp restart | Two separately encoded three-picture inputs start at PTS zero; drain/destroy and fresh contexts separate their epochs | Explicit driver-declared boundary, not automatic live-protocol discontinuity detection |
| Wrong cross-epoch layer | Replace epoch 1 EL with validly decodable epoch 0 pictures having the same timestamps | Successful decoding must still fail native-picture association |
| Missing stored duration | A wrapper omits both duration fields while decoder APIs infer different durations before and after seek | Successful decoding cannot certify source presentation intervals |

There are 21 accepted picture/RPU associations: twelve VFR pictures, three
post-seek pictures including preroll, and six across the two timestamp epochs.
Five deliberate negatives cover missing resets for seek and epoch transition,
cross-epoch EL substitution, swapped RPU and unspecified stored duration.
No additional GPU arithmetic or movie-quality comparison is claimed.

## 2. Duration evidence and seek policy

The compound VFR wrapper deliberately authors intervals of 40, 70, 30, 90,
70 and 41 ms. The first five come from the declared and observed PTS gaps;
the final 41 ms is an explicit synthetic terminal policy. A bounded independent
EBML reader checks stored DefaultDuration/BlockDuration and timestamps against
demux, splitter and decoder observations. Initial and post-seek values agree.

The original encoded inputs have nominal 24 fps duration metadata. The new
wrapper's 40,000,000 ns default supplies its first interval; other intervals
have explicit BlockDuration fields. Its nominal 25 fps is authoring metadata,
not an inferred rate or a claim that the original nominal durations survived.

The retained negative reproduces a harness-authoring omission: copying codec
parameters and time base without stream rates produced neither stored duration
field. APIs then reported 41 ms initially and 33 ms after seek. This is not
evidence of an FFmpeg defect. The private fallback branch was not instrumented;
the control establishes why inferred API duration alone is insufficient.

The seek control labels the 140 ms picture as preroll and selects pictures
whose PTS is at least 200 ms for display. The 140 ms picture's 90 ms interval
contains the seek target. Choosing whether to display that containing picture
is a separate product policy, not settled by this control. Accepted decoder
pictures and emitted presentation pictures are distinct sets.

## 3. Replay and failure evidence

The [retained bundle](../../tools/dv_quality/backends/timeline/README.md)
provides the source pins, dependency lock, commands and evidence inventory.
Its prerequisite is the approved decoded-layer replay. The lock checks the
FFmpeg static prefix, source evidence and generated RPU fixtures before copying
and checks copied artifacts again. This reuses the verified FFmpeg build;
it does not claim another fresh upstream rebuild.

```bash
export PYTHONDONTWRITEBYTECODE=1
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
sh tools/dv_quality/backends/timeline/replay.sh \
  "$scratch/timeline" "$decoded_replay" \
  tools/dv_quality/backends/decoded-layers
```

`decoded_replay` is an absolute path to the approved prerequisite matching the
reviewed dependency lock. The replay requires Linux ARM64 Docker support and
writes to a new scratch directory. Helpers compile with denied warnings against
the pinned FFmpeg static prefix. Network-disabled containers have two-CPU and
two-GiB limits. The inherited base image's live package-resolution limitation
remains; this is not a fully locked operating-system image.

The checker requires ordered packet, split, frame and reader-boundary evidence;
seek/reset must precede consumption in the new epoch, and required drain and
fresh-context events cannot be omitted. Strict field types and finite numbers
prevent malformed evidence from becoming accepted timing. Negative controls
must fail for their expected association or lifecycle reason.

The shell driver records actual decoder status for each case. Failure injection
runs the real decoder through its output writes, then exits 37 or crashes; even
complete-looking files cannot turn either outcome into a successful receipt.
The final exact-source replay passed 26 checker/dependency tests and four real
post-write exit/crash controls. Independent adversarial review verified all
63 bundle ledger entries and 1,094 selected runtime artifact hashes. Those
counts do not claim to inventory every upstream source or intermediate file.

The retained receipt SHA-256 is
`0e7ac1551ffca1df85474a9a3d8c1eeb965fe91b3a8507ab45fcd56310861560`;
the bundle ledger SHA-256 is
`da66738e5f568bb17e71f4ad83c85fb9dfec358279a6b78aad54ab5795c02de6`.
The receipt retains its pre-review state; this document records the subsequent
independent approval without rewriting the frozen evidence. The final replay
explicitly disables Python bytecode in both host and container environments.

To rerun the checker/dependency mutations against the generated replay:

```bash
python3 tools/dv_quality/backends/timeline/test_timeline.py "$scratch/timeline"
```

The replay itself runs the four driver controls inside its container. They
require that environment; the checker command alone does not rerun them.

## 4. Remaining acceptance

All RPUs in this cohort are fresh. Source inspection of FFmpeg's reuse cache
and flush behavior does not replace an executed legitimate-reuse fixture.
Reuse, broader seek/GOP patterns, presentation at the seek target, live
discontinuities, timestamp wrap, format changes, dropped/late layers,
cancellation, backpressure and production fallback remain open.

These controls do not qualify a production backend, HDR10-E badge, general
Dolby Vision conformance, physical display fidelity or real-time performance.
