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

## 8. Extension graph correctness — 2026-10-08

The [extension receipt archive](evidence/macos-video-20261008/route-correctness-evidence.tar.gz)
retains 90 command, observation, stderr and experiment-driver files plus its
manifest. Its size is 25,487 bytes; SHA-256 is
`84eff0a3a10016b23fa79227ebc164285c550c8fe3d031125aa2e94f83aaf6f8`.
Every retained file's length and SHA-256 were checked after archive creation.
The manifest separately records original and retained hashes because private
experiment paths were normalized. Encoded media and raw pixel files are
excluded. The original lawful source fixtures are committed with `b8b7a9574`.

The experiments used the same official Jellyfin binaries identified in §2.
Native C/Rust compilation ran concurrently, so recorded wall times are
execution diagnostics only. This is standalone graph evidence supporting the
foundation integrated in `8a78affb7`, not final daemon or client acceptance.

| Graph | Observation | Remaining limit |
|---|---|---|
| HLG → Metal SDR | 12 progressive frames at 12 fps, BT.709 limited range; gray codes 16, 38, 65, 128, 156, 182, 202, 213; analytic 203-nit reference-white patch maps to 156 | No calibrated display, mastered-content or temporal qualification |
| BWDIF TFF/BFF | Send-frame produces 12 frames at 12 fps; send-field produces 24 at 24 fps; PTS advances exactly by output cadence, progressive BT.709 limited-range signaling | No paced broadcast, real combing or caption acceptance |
| SDR8/SDR10/HDR10 with PGS | Half-alpha red sample YUV 39/115/185 in frames 3–8 only; background 16/128/128 before/after | No complete seek, cancellation or subtitle-rendering matrix |
| HEVC SDR | Main, 8-bit, BT.709 limited-range output | Negotiated production route and client playback remain separate work |
| HEVC HDR10 | Main 10, PQ/BT.2020 limited range; mastering-display and content-light metadata retained exactly | Actual HDR presentation and client playback remain unqualified |

Two fixture defects were corrected before accepting these observations.
The interlaced encoder needed explicit VUI signaling; output retagging was
not used to hide a missing source declaration. The SUP demuxer normalized
its first PTS from 0.25 seconds to zero; an explicit 0.25-second mux offset
restored the intended cue interval. The archive preserves corrected outputs,
negative source provenance and the correction command. The original negative
PGS encoded output was superseded during the retry; its sample arrays were
observed in the tool transcript and are not represented as retained raw files.

VideoToolbox frame representation still does not establish physical hardware
HEVC decoding. The strict hardware-required package patch and observed session
property belong to the separate E1 workstream.

## 9. Strict package AC-4 preservation — 2026-10-08

The [paired audio receipt](evidence/macos-video-20261008/ac4-package-preservation.json)
records exact matching commands against the original official package and
compiled strict candidate. Both decoded the first AC-4 program from the public
FFmpeg sample `KDAF.ts`, downmixed to stereo 48 kHz signed 16-bit PCM for five
seconds. Both produced 239,999 audio frames with byte-identical decoded PCM,
SHA-256 `28e6cfb582fa02e02824583e4a4a1d12e95d1ee5a3b67725c718959c697558c7`.

The strict FFmpeg binary SHA-256 is
`91d4f841e0b607bcc2db3c7ea66ddad0dcff0370a39f1f67fb6a6cc1a7f64495`;
the source sample SHA-256 is
`1198188445e56eee0787f2da1864189afb1674d777249091b6dbf2c736c48384`.
The receipt identifies
[FFmpeg's public sample](https://samples.ffmpeg.org/A-codecs/ac-4/KDAF.ts).
No source media or decoded audio is redistributed here. This establishes
standalone preservation for that sample, not daemon pacing, all AC-4 modes,
client playback or performance under load.

**Subsequent P5 limitation:** the package identified above is superseded for
strict P5 admission after a reuse fixture exposed upstream default DM color
substitution. This does not alter its measured AC-4 output; it does mean the
AC-4 receipt must not be presented as qualification of the repaired future
package or of Dolby rendering. The status ledger records the repair and
withdrawn reuse claim.

### 9.1 Repaired final package preserves the same AC-4 output

The [final-package receipt](evidence/macos-video-20261008/ac4-final-package-preservation.json)
repeats only the affected candidate command after the strict Dolby color
repair. The unchanged original-package reference is retained. All four
reported package artifact hashes were independently checked before execution.
The repaired FFmpeg SHA-256 is
`85b1ab4176f12d6319e2b4dc256f360a70043cb71ddf4057e35e709e98ebb123`;
FFprobe is
`3772130dbb03addb834640c75a95d788200cd019831700ab388958d78d413b1f`.

The final package again produces 239,999 stereo 48 kHz audio frames with
byte-identical PCM and the same `28e6cfb5…` hash recorded above. This is
exact final-artifact AC-4 preservation for the five-second standalone sample,
with the same daemon/client/pacing and concurrent-load limitations.

## 10. Native held-source parser — 2026-10-08

The [parser receipt archive](evidence/macos-video-20261008/native-parser-evidence.tar.gz)
retains 33 source, command, output, stderr and receipt files plus its manifest.
Its size is 79,792 bytes; SHA-256 is
`cf95b6cca2427fc0874b0576b8440f722548b48ed9f4204b7358a60417cc07f5`.
All retained lengths and hashes were verified after archive creation.
The manifest separates original and retained hashes, records private-path
normalization, and includes the exact diagnostic harness source and lockfile.
The parser source is from `35f7a8cf6`; the signed harness is 11,205,664 bytes.
The command inventory is reconstructed from executed inline tool commands,
not represented as a standalone driver originally retained at execution.

The final Jellyfin SIMD module is 15,311,140 bytes, SHA-256
`36f7e0298eac14d1a664dfc083702bab812f6c83d196c651420af92f1080f21c`.
It matches native FFprobe on all 600 decoded frame and plane checksums from
the public AC-4 broadcast source. This loaded stress comparison is functional
parity, not runtime qualification; production only invokes `idet` when held
source metadata reports interlace. Ordinary progressive sources use metadata
projection. The production stderr cap remains 16 KiB; the parity diagnostic
uses 512 KiB to retain its frame observations.

| Source | Metadata execution | `idet` execution | Native/Wasm agreement |
|---|---|---|---|
| 1080i MPEG-2 TFF, 300 coded frames at 29.97 fps | 0.001838 s | 2.511859 s | 300 TFF, no other verdicts |
| 1080i MPEG-2 BFF, 250 coded frames at 25 fps | 0.001603 s | 1.517107 s | 250 BFF, no other verdicts |
| Synthetic 4K MPEG-2 field-marked, 240 coded frames at 24 fps | 0.001842 s | 5.900622 s | 167 TFF and 73 undetermined |

Fresh eager compilation took 7.41–7.84 seconds, within its separate ten-second
identity budget. These executions used a signed ad-hoc diagnostic harness
with a 20-second execution cap; each reported representative execution is
below the unchanged ten-second production execution budget. The 4K case
does not qualify 4K HEVC or HDR.

Historical filenames containing `idle` do not establish a globally idle host.
No competing task compiler ran during the final representative timing
sections; preparation and JIT work still contributed to aggregate load.
Load averaged 9.10 before the first group and 22.99 after later preparation
and compilation. Thermal telemetry and peak RSS were unavailable, not zero.
These are bounded feasibility measurements. Actual signed daemon delivery,
fresh installation, sustained capacity and client presentation remain separate
acceptance work.

The copied hardened harness succeeds with the explicit executable-memory
entitlement. Without it, the supported signing preflight refuses execution
instead of letting the kernel kill the process. The implementation plan
records the additional trusted runtime and executable-memory trade-off; this
is not equivalent to retaining all default Hardened Runtime protections.

## 11. Strict P5 reconstruction and metadata controls — 2026-10-08

The [strict Dolby archive](evidence/macos-video-20261008/dolby-strict-evidence.tar.gz)
contains 126 command, fixture-provenance, decoded observation, stderr, driver
and receipt files plus its manifest. Its size is 63,064 bytes; SHA-256 is
`74ad0708a2cbb059375d610e4c83386926b5d1e251a4249129b4be49c7be6d5a`.
Every retained length and hash was verified. Raw pixels, encoded outputs and
executables are excluded; their hashes remain in the receipts. Source paths
are normalized and the manifest distinguishes original and retained hashes.
The exact repaired FFmpeg/FFprobe hashes are in §9.1.

Each of four graphs ran the same 12 cases: software decode with strict CPU
mapper, required-hardware VT decode with strict CPU mapper, software decode
with explicit VT upload and strict Metal mapper, and required-hardware VT
decode with strict Metal mapper. All 48 cases passed their individual
expectations. Hardware session use was observed where the VT decoder was
created; early invalid metadata may reject before session creation.

| Case | Expected and observed result on each graph |
|---|---|
| Fresh per-frame P5 metadata | 24 frames; success |
| Changing DM identifiers and peak values | 24 frames with presentation-bound metadata; success |
| Seek to valid fresh-metadata random access point | 12 frames; success |
| Explicit mapping reuse carrying DM | 24 frames; nonconforming pinned-parser robustness diagnostic only |
| Renderer metadata deleted; first RPU missing/malformed; seek-start RPU missing; first color metadata omitted | Nonzero exit, zero encoded frames |
| Midstream RPU missing/malformed; color omitted with mapping reuse | Nonzero exit after 10 preceding frames; affected presentation time absent |

Eight separate controls exercise `avformat_seek_file` and
`avcodec_flush_buffers` on the same decoder context, with software and required
hardware decoding. Valid fresh metadata resumes; missing random-access RPU
and reuse after cleared state fail. The per-frame DM identifier/peak mapping
remains tied to presentation order. These are actual decoder controls, not
merely separate command invocations presented as flush proof.

The corrected synthetic source uses IPT-C2 VUI matrix value 15. Pinned swscale
cannot perform planar/P010 layout conversion with that color tag, so the
strict layout adapter clears only the matrix tag before rearranging samples.
Required parsed P5 RPU metadata remains the color interpreter. VT-to-Metal
requires no layout adapter. Fresh and varying-DM fixtures have byte-identical
software/hardware decoded planes over all 24 frames: 4,147,200 bytes, maximum
and mean sample difference zero. The same CPU renderer after either decoder
also matches all 2,073,600 bytes. CPU and Metal tone curves are not asserted
to produce identical output.

Forcing generic interpretation with Dolby application disabled changes
908,148 output bytes, with maximum byte difference 157. That is evidence that
RPU application influences pixels, not calibrated perceptual acceptance.
The omitted-color negative specifically prevents upstream default color
substitution from passing as effective P5 metadata. The strict renderer checks
P5 signal semantics rather than blacklisting a single peak value. Ordinary
non-strict routes keep their previous behavior.

These are original synthetic correctness controls run during compilation.
They do not establish mastered-content coverage, production performance,
client presentation, or qualification on another Mac. Runtime observation and
immutable production recipe integration remain separate from this standalone
package evidence.

## 12. Independently observed host-memory HEVC graphs

[The HEVC host archive](evidence/macos-video-20261008/hevc-host-evidence.tar.gz)
retains 27 driver, command-receipt, frame-probe and pixel-observation files
(7,931 compressed bytes; SHA-256
`90c2b937b1ea48c42814c2fd30f78a68051595d0f76a151f56ab67efca561ea6`).
Its manifest verifies each retained file and records its original hash before
task-path normalization. The exact baseline Jellyfin binary hashes and all
21 bounded commands are preserved. This supports the separately observed
host graphs introduced by `432a617c2`; it is not evidence for the final daemon.

Six tuples cover software or VideoToolbox system-memory decode with CPU
scaling and required HEVC VideoToolbox encoding, each for SDR 8-bit, SDR
10-bit and HDR10. Twelve decoded frames per tuple have the promised Main or
Main10 profile, `hvc1` sample entry, canvas, rational cadence, range and color
facts. Gray patches remain within six 8-bit code values of the source;
HDR10 mastering/content-light metadata matches across the decoded frames.
These observations do not borrow native Metal graph acceptance and remain
usable when the native-processing preference is off. They establish synthetic
correctness under concurrent compilation, not throughput, energy use or
client presentation.

## 13. Combined package: moving fields and explicit-SAR P5

[The combined correctness archive](evidence/macos-video-20261008/combined-package-correctness.tar.gz)
retains 90 command, driver, probe and observation files (44,477 compressed
bytes; SHA-256
`7e256e6b1e87927d65a0026a09c61d56cdc369789760aa12da1aa1a57257a966`).
Every retained member has original and normalized hashes in its manifest.
It binds positive controls to FFmpeg
`80416af20a3fa4055b0460cda5deb5a2b59da6abf0e760d98f282e70a09b6708`
and negative BWDIF controls to the earlier `85b1ab41…` binary. Binary encoded
media and raw planes are excluded; their hashes and sampled measurements are
retained. These bounded experiments ran during compilation and establish
correctness, not performance.

### 13.1 BWDIF parameter-buffer repair

The pinned native filter supplied its parameters at Metal buffer index 4;
the shader read buffer index 0. Texture indices 0–3 occupy another namespace.
Correcting that binding preserves the intended algorithm and field association.
Static gray/cadence receipts from the older binary remain execution evidence,
but are explicitly insufficient motion evidence.

Eight raw comparisons cover H.264/MPEG-2, TFF/BFF and frame/field output.
H.264 maximum CPU/native differences are one code value for frame output and
two for field output, with mean differences about 0.001. MPEG-2 maxima are
five to seven codes with means about 0.004. Twelve encoded controls then
verify the actual output: eight software-decode/upload Live graphs and four
H.264 VT-surface file graphs. Moving white markers retain the correct field
association and the expected 12/24-frame cadence. Four targeted controls on
the old package fail the same marker acceptance threshold despite successful
encoding. VT surfaces in these controls alone do not establish physical
hardware decoding.

### 13.2 Square-pixel P5 controls

The synthetic generator now explicitly records SAR 1:1, verified in the
stream and all 24 decoded frames. Production admission requires known square
pixels; the experiment must meet that same contract. Regeneration changes
container/source hashes but preserves every decoded plane for both fresh and
varying-DM sources: 4,147,200 bytes each, byte-identical to the earlier corpus.

The corrected combined package passes 24 selected cases across all four
strict decoder/renderer graphs: fresh metadata, varying presentation-bound DM,
valid seek, renderer metadata removal, missing midstream RPU and omitted
first color metadata. Required-hardware raw-plane equality, same-CPU-renderer
equality and RPU pixel influence controls pass again. This is a targeted
refresh, not a claim that all 48 historical cases and eight flush cases were
rerun on the new binary. Those earlier receipts retain their exact hashes.

The manager also independently repeated the five-second AC-4 candidate decode
on this combined package. [Its receipt](evidence/macos-video-20261008/ac4-combined-package-preservation.json)
records byte-identical PCM against the retained original Jellyfin reference:
239,999 stereo frames at 48 kHz, SHA-256
`28e6cfb582fa02e02824583e4a4a1d12e95d1ee5a3b67725c718959c697558c7`.
The public source sample is referenced by URL/hash and is not redistributed.

## 14. Actual normalized daemon delivery on `46f31ec6e`

[The normalized daemon archive](evidence/macos-video-20261008/normalized-daemon-46f31-evidence.tar.gz)
retains nine allowlisted files (35,566 compressed bytes; SHA-256
`ee97db715e0002d42990a3ec461a447b80d59c146c2557a3db81e0d7fa8d100a`).
It contains the executed driver, argv, sanitized receipt, decoded frame facts,
pixel measurements and the configured-budget diagnosis. Full daemon logs,
configurations, state and authorization/session URLs are excluded. The manager
verified the builder's allowlist hashes before normalizing task paths; the
archive manifest records both versions. This older source predates the final
combined implementation and is not final-candidate qualification.

The signed isolated daemon used the normal library scan, bound source parser,
candidate selection and HTTP delivery with the corrected `80416af2…` Jellyfin
package. It delivered native normalized 2560×1440 output from the analytic
3840×2160 source for 120.010 seconds of paced demand: 60 objects totaling
2,981,519 bytes. Seek/resume requests, served-object retrieval, cancellation and resource-release
controls passed. The periodic analytic artwork does not independently prove
the displayed seek target or frame-accurate client presentation. Switching native processing off retained an existing session's
captured candidate/media while a new request received a distinct CPU plan.
No client-presented-frame or rebuffer claim follows from HTTP delivery.

The CPU reference originally requested eight software threads against the
experiment's configured pool of two. Existing admission diagnostics reported
`over_budget=true`; no FFmpeg child started. Releasing the prior session did
not fix that configuration error. Raising the ordinary configured budget to
eight admitted the comparator without changing production admission or
startup deadlines. A separate stale-lifetime 409 in the driver was corrected
by creating a new playback lifetime through the normal protocol.

Aligned 48-frame outputs use the same canvas, cadence, bitrate and VT encoder.
The gray-patch maximum error is one code value, neutral chroma error zero,
and the saturated color-code bound is eleven. Paired patch differences are
at most one code value. Whole-frame PSNR is at least 51.92 dB and averages
53.23 dB. These measurements support this synthetic comparison, not universal
perceptual acceptance or a throughput claim under concurrent compilation.

A subsequent finite HEVC request produced 48 independently decoded `hvc1`
Main 1920×1080 frames with BT.709 limited-range signaling. Its master endpoint
then rejected initialization data with `hls_init_invalid`. The complete
driver is therefore correctly recorded as failed; the earlier positive M4
phase is retained separately. The init/parser/manifest contract remains a
blocking integration issue until repaired and demonstrated through that same
normal endpoint.

## 15. Correct the embedded HLG and text observations

[The observation-repair archive](evidence/macos-video-20261008/runtime-observation-repairs.tar.gz)
retains 16 command/probe/driver/measurement files (7,490 compressed bytes;
SHA-256 `3a5b62121f34bd4f1763dca414869c8e834c00b0e250b3c929ea8a7f949b1d61`).
Original and normalized member hashes are verified. These synthetic fullgraph
controls use the corrected Jellyfin package during ongoing builds; final
combined daemon observations and performance remain separate.

The HLG source previously omitted SAR while its runtime contract requires
known square pixels. Encoding explicit SAR 1:1 changes the source hash but
leaves every decoded 10-bit plane byte unchanged. All twelve actual graph
outputs now satisfy SAR and the 203-nit reference-white expectation, with
measured output luma 156 on every frame.

The text probe's old peak comparison used a bright background with luma 231;
a correctly rendered cue peaked at 235, below the required increase of five.
Actual cue-versus-blank differences in the stationary region above the moving
marker average about 21 code values for SDR8 and SDR10, versus at most 0.25
for blank controls. The repaired observation uses that scheduled difference
and rejects both an actually absent cue and a cue retained into blank frames.
ASS content, font rendering and production subtitle semantics are unchanged.

The same source commit `21110a5`, integrated by `d92700dea`, retires the obsolete
static interlace runtime assets and registrations. They lacked encoded SAR
and temporally distinct field artwork, so they could neither satisfy nor
prove the new motion contract. The existing genuine woven H.264 TFF/BFF
controls now solely qualify ordinary BWDIF; its strict field-motion checks
remain intact. Historical receipts keep the older limited evidence visible.
