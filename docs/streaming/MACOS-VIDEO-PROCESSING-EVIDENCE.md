# macOS processing — initial experiment evidence

**Status:** open; feasibility evidence, not release qualification · **Observed:**
2026-10-07 · **Scope:** M0/M1 of the
[implementation plan](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md).
The [status ledger](MACOS-VIDEO-PROCESSING-STATUS.md) tracks every outstanding
implementation, review and qualification item.

## 1. What the experiment establishes

On one Apple M3 Max, retaining VideoToolbox surfaces for SDR scaling and
scaling HDR10 at 10-bit precision before Metal tone mapping substantially
reduced child CPU time for synthetic 4K24 inputs. These observations justify
continuing integration. They do not establish perceptual quality, deployed
daemon performance, client behavior, hardware HEVC reconstruction, energy
savings, zero-copy operation or supported concurrency.

| Workload | Baseline throughput | Candidate throughput | Median paired CPU reduction | Median paired throughput change | Disposition |
|---|---:|---:|---:|---:|---|
| SDR scale 4K → 1080p | 10.565× | 10.640× | 87.80% | +0.71% | Synthetic CPU feasibility: 5/5 pairs meet the CPU/throughput criterion |
| Original HDR10 comparison | 9.562× | 7.735× | 97.11% | −19.11% | Invalid for acceptance: stale metadata in baseline; unequal processing order |
| Revised HDR10 comparison | 9.341× | 9.485× | 97.40% | +2.14% | Synthetic CPU feasibility: 4/5 pairs meet the CPU/throughput criterion |

Baseline/candidate throughput columns are medians of each graph's five warm
runs. Paired percentage columns are medians of the five pairwise changes,
so dividing the two throughput medians need not reproduce the paired result.
Revised HDR pair four was 5.51% slower, outside the 5% per-pair bound. It is
retained explicitly; the result meets the design's four-of-five consistency
threshold without discarding that row. This is not a claim of higher
throughput across all runs.

All twelve SDR outputs and all twelve revised HDR outputs passed the
encoded-output contract: 1,440 decoded frames, 1920×1080, 24 fps, monotone
timestamps and BT.709 limited-range output without stale HDR side data.
Twelve means the separate first-use pair plus five warm pairs. Original
HDR CPU outputs failed the metadata contract; those runs remain in the
evidence. First-use process runs are not included in warm medians, and no
cold-file or shader-cache state is claimed.

## 2. Fixed environment and treatment

The host was native arm64, Mac15,9 / Apple M3 Max with 64 GiB RAM, macOS
build `26A434`. Power mode and thermal state were not measured. Hardware
runs were serialized without concurrent builder compilation. The exact
deployed daemon was unavailable; every baseline here is source-reconstructed,
video-only MP4 processing using the same official Jellyfin package as its
paired candidate. No installed FFmpeg or live service was changed.

| Implementation fact | Observed identity |
|---|---|
| Jellyfin source tag / commit | `v8.1.3-1` / `253db2a7b0a8045c54ce68ce33d7f601229b1822` |
| Official arm64 archive SHA-256 | `22445d7299742749ad2eeb9ce87963d50def0357e45b3e6c7b69987c8365dbf6` |
| FFmpeg SHA-256 | `f043b55df439a90743faf539ce88b723c84eb8baa0ae3323a894c87b459b85ef` |
| FFprobe SHA-256 | `6e5f60f7387917b33e3c4dfdf1e2d13cc63a1e928c0ac75c4f0dcd89f1c2a90d` |
| Patch-set digest | `4894a7b7fd414029054bd10931147c3b30bb7da61fa934a5b67f7a5576eb784f` |
| Binary deployment target / SDK | macOS 12.0 / SDK 26.5 |

The acquisition script verifies the complete official archive and retains
source/patch provenance. Thirty-eight incumbent capability declarations were
checked, including AC-4 and Dolby processing. Declaration checks are not
sample-level acceptance for either codec. Local Homebrew FFmpeg was not an
experiment baseline or runtime dependency.

Each sustained source is 60 seconds of 3840×2160 at 24 fps: analytic gray and
color patches, a moving marker, and a scene change every five seconds. SDR
uses 8-bit HEVC and HDR10 uses 10-bit HEVC with actual ST 2084 code values,
explicit VUI, mastering metadata and content-light metadata. The simple
patterns are not representative compressed movie complexity.

The output controls were identical within each comparison: H.264 VideoToolbox,
hardware encoding required with `-allow_sw 0`, bitrate/maxrate/buffer
8000k/12000k/16000k, no B-frames, forced keyframes every two seconds, BT.709
limited-range output. Audio, HLS, client startup and subtitles are absent.
Baseline VideoToolbox decode exposes host frames; candidates request
`videotoolbox_vld`. That representation change is part of the treatment.
VideoToolbox surfaces do not prove the HEVC decoder actually used hardware:
the pinned implementation requests hardware decoding but permits fallback.

The retained normalized graph manifests contain exact argv arrays:
[original comparisons](evidence/macos-video-20261007/sustained-graphs.json)
and [revised HDR](evidence/macos-video-20261007/revised-hdr-graphs.json).
The revised Metal filter chain is:

```text
scale_vt=w=1920:h=1080:format=p010le,tonemap_videotoolbox=tonemap=bt2390:tonemap_mode=itp:transfer=bt709:matrix=bt709:primaries=bt709:range=tv:format=nv12:apply_dovi=0
```

CPU Hable and Metal BT.2390/ITP are different tone-mapping treatments.
Their output cannot be declared perceptually equivalent from geometry,
tags, average luma, or CPU measurements.

## 3. Root causes exposed and corrected in the experiment

The original CPU graph produced BT.709 output but retained mastering-display
and content-light side data in the stream and decoded frames. Removing only
those consumed metadata types at the actual HDR-to-SDR conversion boundary
cleared both, while preserving the unrelated encoder user SEI. A three-frame
before/after smoke produced identical decoded YUV pixel hashes:
`6bc467183e75ca2b19bda37be95ff21b9fc98e19ecc4a138fc6221a5dc85311c`.
The exact typed cleanup is:

```text
sidedata=mode=delete:type=MASTERING_DISPLAY_METADATA,sidedata=mode=delete:type=CONTENT_LIGHT_LEVEL
```

The revised comparator includes this proposed correction and is explicitly
labeled as such, not as the deployed graph. Application integration belongs
in a separate corrective commit with a scoped metadata-policy identity
revision; retaining the old cache key for changed encoded bytes would be
incorrect. SDR and HDR-preserving routes must retain their existing identity.
The correction is limited to the reproduced CPU + zscale HDR-to-SDR route.
`tonemapx` already removes these consumed metadata types; its Dolby routes
and the Metal path do not need additional cleanup or identity rotation.

The original Metal graph tone-mapped the full 4K frame and scaled afterward,
while the CPU graph scaled first. The candidate therefore tone-mapped four
times as many pixels. Scaling first into `p010le` preserved 10-bit HDR input
and moved the Metal mapper to the same 1080p work size. Three-frame tiny and
4K smokes passed output checks; the tiny HDR gray ramp remained unchanged.
This is a processing-order correction, not a new retry or watchdog.

## 4. Continuous VOD and early P5 diagnostics

The existing continuous AVC High 5.0 VOD envelope was exercised with the
observed SDR and HDR10 graphs. Three-frame smokes retained the producer's
trim, frame rate, SAR, padding, timestamps and color normalization: 1080p
H.264 High level 5.0, BT.709 limited range, PTS 0/1/2 on a 1/24 time base,
with no stale static HDR metadata. These checks justify admitting the
existing envelope in the shared resolver; they are not seek, fragmented
VOD or client playback qualification.

An early four-way P5 diagnostic used the tiny artificial
[FFmpeg FATE fixture](https://fate-suite.ffmpeg.org/mov/dovi-p5.mp4),
4,182 bytes with SHA-256
`11fe599fd77e31e26fbf855bae1cd9931df9f261a0a7b1dce9fad9b236677c4b`.
The fixture media is not redistributed in the evidence archive.

| Decode route | Renderer | Observation on all ten frames |
|---|---|---|
| Software HEVC | CPU `tonemapx` | Parsed Dolby metadata and 162-byte RPU before renderer; consumed afterward |
| VideoToolbox | CPU `tonemapx`, via P010 download | Same metadata and PTS association observed at renderer input; consumed afterward |
| Software HEVC | Metal, via P010 upload | Same metadata and PTS association observed at renderer input; consumed afterward |
| VideoToolbox | Metal | Same metadata and PTS association observed at renderer input; consumed afterward |

The ten pre-renderer PTS values were 0, 512, …, 4608. All four outputs had
ten 160×90 H.264 BT.709 limited-range frames, with no RPU/parsed Dolby side
data after rendering or in the output stream. Only the initial encoder user
SEI remained. Exact argv and observations are retained.

This is evidence against a universal claim that VideoToolbox always loses
Dolby frame metadata. It is not proof of physical hardware HEVC reconstruction,
RPU influence on pixels, absence of OS-level double processing, valid reuse
after seeks/reordering, strict rejection of missing metadata, performance or
real-content visual quality. In particular, this artificial sample does not
satisfy the plan's RPU-positive visual criterion. Production Dolby decoding
restrictions remain unchanged pending E1's full contract.

## 5. Isolated daemon delivery and the remaining native source boundary

An isolated daemon used its own loopback ports, database, synthetic library,
cache and explicit Jellyfin binaries. It advertised no service and changed no
installed configuration. The saved preference round-tripped false/true/false/true
while probes were pending, survived restart, and remained true after an admin
reprobe (HTTP 202). Both graphs became available. Corrupting an owned cached
fixture caused content-verified repair on the next probe. Both shutdowns exited
successfully.

Separate scanned 60-second SDR and HDR10 sources exercised the actual session
API, canonical resolver and encoded VOD producer. The selected pipelines were
`vt_scale_sdr` and `vt_tonemap_metal`. Each initialization object and first media
segment returned HTTP 200; Jellyfin FFprobe decoded 48 progressive, square-pixel
1920×1080 H.264 High level 4.0 frames at 24 fps, BT.709 limited range. The HDR
output had no mastering-display, content-light or Dolby side data. These are
produced-byte checks for the existing non-normalized VOD path, not display,
seek/resume or long-session acceptance. The public live presentation returns
`410 live_presentation_removed`; no rolling client result is claimed.

The experiment binary identifies its exact SHA in each receipt. It was built
on `a17de21fe` plus the daemon integration changes, before advisory wording,
test-only lint corrections and the AR-01–AR-05 implementation-review repairs.
The measured processing graphs are unchanged by those repairs, but these
receipts do not execute the final dependency inventory, recovery or producer
identity fences. This is behavioral experiment evidence, not final-branch
runtime qualification, compilation or merge qualification.

**Normalized continuous VOD remains unavailable on native macOS.** Its bound
source probe deliberately requires Linux's executable/dependency confinement.
Both executable admission and production child launch reject unsupported Mac
execution. The existing fixture-only Mach-O launcher is not a production
substitute. The held source descriptor, observation cache, deadlines and reap
ownership are reusable; removing those refusals would still leave executable
binding and descendant containment unproved.

A read-only architecture investigation found no small Apple-supported replacement
matching all of those guarantees. App Sandbox supports inherited child execution;
launch/library constraints constrain code properties but do not supply the
current subsequent-exec or process-group escape denial. The existing immutable
file flag is owner-clearable. A signed, minimal parser package therefore needs
a separate launch/confinement design before this route can be admitted. Apple's
[helper-tool guidance](https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app),
[launch constraints](https://developer.apple.com/documentation/security/defining-launch-environment-and-library-constraints),
and [DTS guidance on unsupported custom sandbox profiles](https://developer.apple.com/forums/thread/661939)
explain why simply accepting Mach-O or using a custom Seatbelt profile is not
qualified parity. No source-verification bypass was added.

## 6. Retained receipts and reproduction

Readable summaries preserve both the
[initial results](evidence/macos-video-20261007/m1-initial-summary.json) and
[revised HDR results](evidence/macos-video-20261007/m1-revised-summary.json).
The [archive manifest](evidence/macos-video-20261007/MANIFEST.json) hashes the
[compressed raw evidence](evidence/macos-video-20261007/raw-experiment-evidence.tar.gz).
It contains complete run receipts, inventory, stdout/stderr, negative HDR
rows, native smoke observations, package provenance and synthetic-source
generation/observation helpers. An internal manifest records original and
retained SHA-256 values. Only task paths and the operator home were replaced
with neutral paths. Media and downloaded binary/source archives are excluded;
source hashes and the generation recipe remain.

Extract into a new task-owned directory. `/experiment` in retained commands
is a neutral replacement for the original task root, not an installed path.
The benchmark generation helper uses the committed smoke generator's transfer
and matrix functions and explicit absolute Jellyfin binary paths:

```bash
python3 macos-fixture-benchmark-generate.py \
  --ffmpeg "$JELLYFIN_FFMPEG" --ffprobe "$JELLYFIN_FFPROBE" \
  --smoke-generator "$SOURCE/scripts/generate-macos-video-fixtures" \
  --output "$NEW_CORPUS" --seconds 60 --deadline-seconds 600

"$SOURCE/scripts/bench-macos-video" run \
  --ffmpeg "$JELLYFIN_FFMPEG" --ffprobe "$JELLYFIN_FFPROBE" \
  --corpus "$NEW_CORPUS/manifest.json" --graphs "$GRAPH_MANIFEST" \
  --baseline "$BASELINE_ID" --candidate "$CANDIDATE_ID" \
  --fixture "$FIXTURE_ID" --duration 60 --repetitions 5 --timeout 300 \
  --source-commit "$SOURCE_COMMIT" --output-dir "$NEW_RESULT_DIRECTORY"
```

Use `sdr2160p24` with `cpu_sdr_1080` / `vt_sdr_1080`, or `hdr2160p24`
with `cpu_hdr10_metadata_corrected_1080` / `vt_hdr10_scale_before_tone_1080`.
The original HDR graph IDs are retained solely to reproduce its negative
result. Run receipts identify source commit `0cd06e92b` and the revised
harness SHA where applicable; integrated commit `308921d3f` records the trim/probe
fix used for revised measurement. Reproduction requires actual Mac hardware
access, including IOSurface access; the tool sandbox denied that access.

For the isolated daemon checks, the archive also retains `daemon-evidence/exercise.py`
and `exercise-delivery.py`. Each creates a new private output directory, chooses
loopback ports, generates bootstrap credentials in memory and shuts down its
owned daemon. Do not point these at an installed service or an existing data
folder. Use the delivery driver once per synthetic source:

```bash
python3 daemon-evidence/exercise.py \
  --binary "$BUILT_PLURXD" --package "$JELLYFIN_PACKAGE" --output "$NEW_SETTINGS_RUN"
python3 daemon-evidence/exercise-delivery.py \
  --binary "$BUILT_PLURXD" --package "$JELLYFIN_PACKAGE" \
  --source "$SYNTHETIC_SOURCE" --output "$NEW_DELIVERY_RUN"
```

Only receipts, decoded-frame observations and selected planner-log excerpts
are retained from those runs. Database/configuration files, generated credentials,
full daemon logs and encoded media are excluded.

### Runtime dependency scope after implementation review

Runtime admission is limited to the measured Jellyfin packaging model: both
FFmpeg and FFprobe carry non-Apple dependencies statically and link only canonical
Apple system libraries/frameworks. The runtime inventories both programs with
`/usr/bin/otool`, binds those inventories and the OS build, and rejects unresolved
or non-system dependency closures and `DYLD` overrides. Other dynamic Jellyfin
packages are not qualified by these experiments. The saved processing switch is
still accepted; an unverified implementation retains the incumbent route.

A functioning `otool` is a runtime inventory requirement and may require Apple's
[Command Line Tools](https://developer.apple.com/documentation/xcode/installing-the-command-line-tools/)
on a fresh machine. No installer or network fallback is added.
Clean-install packaging acceptance remains open. The identity deadline covers
asynchronous resolution and inventory; cancellation waits for an already-started
private output write to settle before cleanup. An in-flight filesystem syscall
can extend that settlement beyond the nominal deadline; no detached writer is
left able to publish after cleanup.

## 7. Evidence still owed

Five-second steady windows are sparse or absent because these simple 60-second
sources encode in about six seconds. Whole-run CPU/throughput observations
remain valid, but neither realtime capacity nor a sustained thermal envelope
is qualified. Startup distributions, one/two/four-session load, 30-minute
soak, calibrated visual review, full VOD behavior, seek/resume, cancellation,
failure recovery, AC-4/P5 sample acceptance, second-generation Apple Silicon
and Intel Macs remain open. Peak RSS, energy and copy instrumentation are
unmeasured, not zero. The Developer readiness report must describe those gaps
without preventing an operator saving the processing preference.
