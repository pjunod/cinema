# macOS processing — experiment and delivery evidence

**Status:** open; each result is bound to its stated source and workload · **Observed:**
2026-10-07–08 · **Scope:** initial experiments, delivery and required follow-ups in the
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

## 16. Strict P5 shared encoder projections

[The P5 projection archive](evidence/macos-video-20261008/p5-projection-correctness.tar.gz)
retains 113 files (195,509 compressed bytes; SHA-256
`e043104399de5b0179d99c357da4607848473f7b6e2ffaef5438b1774acf60df`).
Unlike the earlier text-only archives, it includes the tiny original-synthetic
encoded outputs and raw decoded/predecessor planes, alongside commands,
probes and logs. Their CC0 provenance is the committed original synthetic
corpus. The manifest verifies every member; textual task paths are normalized.
An earlier encoder-variant receipt remains explicitly historical and is not
counted as the final shared encoder projection.

All 45 controls pass on the corrected `80416af2…` package: nine source cases
across software decode/CPU render with software or VT encoding, required-VT
decode/CPU render, software decode/upload/Metal render, and required-VT
decode/Metal render. Positive controls use the shared encoder arguments and
produce 24 SDR H.264 frames at 12 fps with the expected signaling, cadence
and neutral-patch values. CPU and Metal curves are judged against their own
expectations, not asserted pixel-identical.

Negative controls inspect strict decoder/renderer raw output rather than
encode malformed content: first/seek-start loss yields no frame; midstream
missing or malformed RPU and omitted color metadata produce at most ten valid
predecessor frames, a required diagnostic and nonzero exit. This preserves
an observable boundary at the affected frame. The initial sandbox-denied
IOSurface attempt is not accepted hardware evidence.

The related production source is `f57040d83`, integrated by `3ea037239`.
The daemon's embedded helper must additionally verify held-fixture source
codes, access-unit/presentation metadata association and aggregation of every
control before reporting availability. This standalone receipt does not
substitute for that final inventory. It was collected under concurrent build
load and makes no performance claim.

## 17. HLG burn graphs and HDR interlace boundary

[The HLG and interlace scope archive](evidence/macos-video-20261008/hlg-burn-interlace-scope.tar.gz)
retains 14 files (22,995 compressed bytes; SHA-256
`48e98704dd96c4bcc4f2e0aacf11b24c5c22da6a03d35cac2177529b76fb228c`).
It includes complete HLG text/bitmap graph receipts, the original woven-field
source generator and manifest, four hardware-decoder negative logs/probes,
and the original six-second HDR/AAC API source generator and receipt.
Every retained member is hash verified.

The corrected package independently passes both HLG burn graphs: twelve
frames preserve the expected reference-white luma 156, with scheduled text
or bitmap cue observations. The HLG fixture explicitly encodes SAR 1:1; its
raw ten-bit planes are unchanged by that signaling repair.

Actual ten-bit woven H.264 PQ/HLG sources in both TFF and BFF order fail
VideoToolbox decoding on this host: all four attempts exit 69, return zero
raw bytes and report decoder error -8969 with NULL images. Consequently,
HDR interlaced input retains the incumbent route. Unreachable HDR BWDIF
classes were removed rather than advertised with a permanently missing
observation. This is evidence for these exact source/package/host tuples,
not a universal claim about all Apple hardware or HDR codecs. Ordinary SDR
BWDIF retains its independently proven moving-field controls.

The final supported inventory has 22 graphs. Standalone controls do not
replace the final combined daemon's embedded inventory or API qualification.

## 18. Selected P5 graph observation — review B1

[The selected-graph observer archive](evidence/macos-video-20261008/p5-selected-observer-evidence.tar.gz)
retains 120 files (273,763 compressed bytes; SHA-256
`654cd97d1767244dc5108600b8b7565f1f354f3be976a81670124779c6f7cfd9`).
Commands, INFO logs, encoded outputs, raw planes, observations and original/
normalized member hashes are retained. The media remains original synthetic
CC0 material. Source commit `a0a3734b20e6394fe9453be6b3a5f4b3513e0d05` is
independently accepted for B1; these controls use package `80416af2…`.

All 45 existing controls pass. The ten positive source/tuple pairs now expose
24 frame records from `showinfo=checksum=0` immediately before the actual
selected renderer, including opaque VT frames without an extra download.
The observer binds exact PTS, format, raster, SAR and Dolby header/DM fields
to the intended frame; independent ordinary FFprobe output is only corpus
validation. Six colored patches are checked on every encoded output frame.
Their bounds are derived from the analytic source RGB values and BT.2020
to BT.709 conversion with an explicit gain/quantization allowance, rather
than fitted to these observed output values.

Sixteen actual-artifact mutations exercise stale, shuffled, missing and
duplicate metadata and corrupted color while retaining the gray controls.
The independently reviewed bounds reject neutral, swapped-chroma and raw
IPT interpretations. They establish fixture color interpretation, not an
exact tone curve, calibrated visual acceptance or pixel influence from the
tiny per-frame source-peak changes. The separate apply/no-apply experiment
retains its own scope. All 35 strict-loss controls still enforce nonzero
exit, the required diagnostic and absence of the affected frame.

The first external observer incorrectly matched `s:` inside `pts:`. Its
failed receipt and executed driver remain in the historical subdirectory.
Whole-token parsing was corrected before the accepted run and is enforced
in the production observer. This is not a performance run; concurrent build
load is recorded explicitly.

## 19. Legal trailing Dolby NALs — review B2

[The terminal-NAL archive](evidence/macos-video-20261008/p5-terminal-nal-evidence.tar.gz)
retains 75 files (509,719 compressed bytes; SHA-256
`5d2f4d5a007c1ce5b938dab00820128b9cfc10d8429e3911daa438061f914054`).
It includes five tiny original synthetic inputs, their hash-bound generator,
exact commands, old-package failures, software/required-hardware receipts,
per-frame plane checksums and PTS, source-equivalence/warm-build provenance
and AC-4 preservation. Raw planes are not retained; `framemd5` records the
actual comparisons. Text paths are normalized and original byte hashes are
retained; normalized driver copies are explicitly distinguished from their
executed originals. No public AC-4 media, native binaries or full source
offers are embedded in this archive.

The old `80416af2…` package decodes the original 24-frame P5 source, but
legal EOS, EOB or combined markers after the final access unit's RPU each
yield 21 frames and exit 183 under the exact strict production flags. The
upstream extractor inspects only the literal final NAL. Source correction
`184c937d45f1f348392dca048c72a666942ceecc` skips only trailing EOS/EOB to
locate the last non-terminator NAL within the current packet; existing RPU
position, size, layer, temporal and strict metadata checks remain intact.

The rebuilt Jellyfin package has FFmpeg SHA-256
`c8c4b5259b8129d4240599e16e57e9286b2e3c0ca5076e91fa6055b5872ab4c8`
and patch digest
`6987232e27bb1657a6a007ca41a5712c5899110079ecf545b7e3b75f99dd94e4`.
Its 10,602 freshly prepared source files are checked against the warm private
build lineage; complete corresponding source offers remain with the package.
The source parser module is unchanged. No installed package was replaced.

Both software and required physical VideoToolbox decoding now produce all
24 frames/exit 0 for each legal suffix. Within each decoder, plane checksums
and PTS match the original source. A truly missing final RPU still yields
21 frames/exit 183. Nine software-decode/CPU-render strict source cases also
retain their positive and loss-boundary results. Initial sandbox IOSurface
refusal and an unsupported IPT planar conversion remain separate diagnostics,
not accepted hardware controls.

The new package's five-second AC-4 stereo 48 kHz signed-16-bit output remains
byte-identical to the previous package: 239,999 frames, 959,996 bytes, SHA-256
`28e6cfb582fa02e02824583e4a4a1d12e95d1ee5a3b67725c718959c697558c7`.
This preserves the existing bounded sample claim, not full daemon A/V or
all-broadcast acceptance. Independent review accepts the native correction;
terminal runtime registration and final daemon qualification remain separate.

## 20. Final terminal-NAL runtime union

Source `54c3a3fe8122e8d15fcf1ee12ad3454e062cd35c` extends the runtime corpus
with original EOS, EOB and combined terminal positives. Twelve sources across
five decoder/renderer/encoder tuples yield sixty controls. All pass on final
Jellyfin package `c8c4b525…`, including every original strict-loss negative,
selected pre-renderer PTS/metadata associations and independent color bounds.
The existing 210-second aggregate deadline and 256 KiB corpus cap are unchanged.
Concurrent compiler work makes this correctness evidence only.

[Final runtime evidence](evidence/macos-video-20261008/p5-terminal-runtime-evidence.tar.gz)
contains 173 original synthetic artifact/driver/receipt files, 548,548 bytes
compressed, SHA-256
`492693a315475061865120052dfe00d17abeb290011b8e2a2b6606711552519e`.
Every manifest member's size and hash was verified after writing. The narrow
independent review accepted this registration without new findings. Final
packaged-daemon inventory and normal-API delivery remain separate checks.

## 21. Final source compilation and regression references

Frozen source `3269bbcd984fe08a554c330d01d0a6a0fdf7e7a0` passes Linux
Rust 1.97.1 workspace all-target check (269.79 seconds) and Clippy with denied
lints (275.69 seconds), including `plurx-core/hiqlite-store`. Only committed
source entered the compiler container. The compiler's known `fullfp16`
target-feature warnings are retained in the logs. No units were executed.

The complete Apple subtree is unchanged from its successful iOS/tvOS build,
and the Android subtree is unchanged from its successful app, instrumentation
and JVM regression-source compilation after C1. These passes are retained by
source equivalence, not re-executed. All 67 unique final regression references
resolve through the repository's actual static resolver. One internal Python
class-qualified trailer was normalized to the supported `path::test_name`
spelling in the final PR/landing packet; the valid anchor already existed.

[Final compilation evidence](evidence/macos-video-20261008/final-compilation-evidence.tar.gz)
contains 13 receipts/logs/driver records, 32,052 compressed bytes, SHA-256
`53736d72778003b39aa0d153b77eee00cd3d604264d3b8d04430f415569afcfa`.
All manifest member hashes and sizes were verified after writing. This is
compile/lint and static-reference evidence, not unit execution, physical
client playback, or a complete promotion-policy receipt.

## 22. Normal API delivery and the scanner resolver defect

The signed `a2ee4a221` daemon with final Jellyfin FFmpeg `c8c4b525…` reached
all 22 graph classes through embedded runtime observations. Its normal API
checks passed normalized 1440p SDR, 120.003 seconds/60 objects of paced HTTP
delivery, source-aligned seek/resume timestamps, cancellation, finite HEVC
Main output and ordinary Auto AVC/HEVC preference. Forty-eight paired decoded
CPU/native frames have at most one code value of patch difference, minimum
PSNR 51.92 dB and mean 53.23 dB. Genuine 4K PQ input also produced normalized
1440p SDR and separate 1080p Main10 PQ/BT.2020 HEVC with matching master codec
information. These are media/API observations, not client presentation.

Normal finite P5 delivery passed with the strict captured package policy and
all 24 served 256×144 SDR frames. This finite manual route legitimately has
no canonical normalized candidate binding. Two initial driver failures are
retained: an incorrect binding assertion and an incorrect report-field name.
Neither required application changes or relaxed strict-route checks.

AC-4 exposed a real application defect. The normal scanner used ambient
FFprobe 9.0.1, yielding zero/unknown AC-4 channels and HEVC extradata size 102.
The same source, probed by bundled Jellyfin, yields six channels and extradata
size 99. Playback's held parser used Jellyfin, so source attestation correctly
refused the inconsistent catalog facts with `vod_source_rescan_required`.
Reanalyzing through the same incorrect resolver would repeat the mismatch.
The ambient tool is evidence of the defect, not an accepted package baseline.

Repair `4adb34c022fda0168dc9d766988ae9a258c0c53a` moves executable selection
into the existing core process layer. Scanner, local/book artwork and daemon
wrappers share nonempty explicit override → macOS/Windows executable sibling
→ PATH fallback. Linux defaults remain unchanged. Source confinement and
attestation checks are untouched. Pinned compilation and the normal hook
passed; targeted independent review accepted the repair. New-artifact AC-4
qualification is still required. Existing `is_file()` behavior follows a
symlink to a regular target; this repair introduces no no-symlink guarantee.

[Normal API and resolver evidence](evidence/macos-video-20261008/normal-api-a2ee-evidence.tar.gz)
contains 24 allowlisted sanitized receipt/driver/frame-fact records, including
both harness failures, the actual AC-4 refusal and paired probe facts. It is
139,784 compressed bytes, SHA-256
`56132c88121f2e682389e1f0daf30f87370848494a45d64f4a07bf3e84db848d`.
Every manifest member was verified. No AC-4 media, authentication, private
state or active session URLs are redistributed. These receipts remain bound
to `a2ee4a221`; they are not relabeled as results from the repaired daemon.

## 23. Actual Live API delivery and source-change diagnosis

The signed `a2ee4a221` daemon and `c8c4b525…` Jellyfin package pass twelve
ordinary synthetic Live cases: progressive, TFF and BFF sources in frame
and field modes, each started twice. Coverage combines six frame cases from
one retained run and six field cases from another; it is not an uninterrupted
twelve-case run. Normal resume rejoins each capability. Actual decoded
256 × 144 BT.709 progressive output is 12 fps for frame mode/progressive
input and 24 fps for woven-field input in field mode. Captured native argv
uses software input decode, upload, native scale/BWDIF, VideoToolbox encoding
and the existing scoped Live caption handling.

MPEG-TS does not report output SAR for this encoder on the observed host.
Bounded controls show native MP4 reports 1:1 while both native and incumbent
CPU-scale MPEG-TS omit it. The container-aware oracle records absence and
checks exact raster/rate/color; the application's source SAR guard and
MP4 observer are unchanged. An immediate restart may receive the existing
ownership decision `settings_conflict` with a draining `retry: now` hint;
the driver follows only that exact hint with an unchanged request for at
most ten seconds, recording each refusal.

EOF produces the existing terminal stream-failure accounting, ended state
and ended resume response. An explicit fresh request reconnects and decodes
144 progressive frames. A separate source-format transition fails the typed
source-change expectation: FFmpeg exits with 178 after native filter
reconfiguration, and generic exit handling outraces the source-change flag.
The accepted ownership repair is documented in review §7.5; its new actual
result must be recorded separately and cannot rewrite this failed receipt.

[Sanitized Live receipt](evidence/macos-video-20261008/live-api-a2ee-evidence.tar.gz)
contains one hash-verified data file (4,939 compressed bytes), archive SHA-256
`9fa50645d929e56b909eb84d7f21e69f94e20dac2ffda76851b5aa5f0218e374`.
Private raw receipt/log/driver hashes preserve provenance without retaining
LAN identifiers, capabilities, credentials, configurations or media. The
receipt also records the unsuccessful tuner-port/harness iterations and
container-aware SAR diagnosis. No physical client, broadcast A/V-sync,
performance or automatic-retry qualification is inferred.

## 24. Cache integrity, refusal and recovery

A task-owned signed `a2ee4a221` package on a disposable 64 MiB HFS+ volume
passes empty-cache reconstruction, exact repair of corrupt fixture bytes,
permission-denied refusal and recovery, zero-capacity refusal and recovery,
and restart with the saved choice and all twenty-two observations intact.
Preparation failures report `fixture_cache_unavailable`, no implementation
identity and no extended graph inventory. They do not override the saved
processing choice. The volume and image are detached and removed afterward;
the host filesystem is never filled.

Two failed external controls are retained. The first expected twenty-two
graph entries even after preparation had correctly failed. The second stopped
filling on a failed 1 MiB write, leaving enough space for the small fixture.
The final capacity-only run exhausts 4 KiB then one-byte writes: 65,138,688
filler bytes, zero available bytes and errno 28 precede the actual daemon
refusal. Freeing the filler restores exact fixture bytes and complete probes.
Previously valid permission and corruption observations are retained; only
the failed capacity control and required recovery/restart are repeated.

[Cache receipts and executed drivers](evidence/macos-video-20261008/cache-api-a2ee-evidence.tar.gz)
contain nine verified files, 6,571 compressed bytes, SHA-256
`64cbe73c758cf667730bb674eee067ff6023e0d5428f4909cefcad35a4210f1a`.
Paths are normalized; private configuration, credentials, logs, binaries and
media are excluded. These receipts remain bound to `a2ee4a221`; later shared
probe-tool resolution and Live termination changes do not modify cache
preparation or graph observation code. This is private-package operational
evidence, not deployed installation or network-isolation qualification.

## 25. Final source compilation and compiler-independent runtime

Source `5e8a075dce5f36f9e259e47ce593e8b3347edfcf` differs only in
documentation from final release source `1bf84609cd0e334933f0a8c568d5a119b89d8549`.
Its source-only Linux archive passes pinned Rust 1.97.1 workspace/all-target
check and Clippy with `hiqlite-store`; known `fullfp16` compiler warnings
remain in the logs. The archive carries neither `.git` nor credentials.
Earlier resolver-source `12fd73ab5` passes are preserved separately.
The final Live repair also passes pinned all-target compilation and its
normal catalog/format/Clippy/JavaScript hook. No unit execution is claimed.
All seventy final regression references resolve statically.

[Final repair compilation evidence](evidence/macos-video-20261008/final-repair-compilation-evidence.tar.gz)
contains fifteen hash-verified files, 11,847 compressed bytes, SHA-256
`6c68a425350e6d1214c9014f58c0e7665194245959ce21624d568a8f643c7bcb`.
It includes source-footprint comparisons, original and final source receipts,
Live builder logs, regression-reference resolution and compiler ownership
cleanup. Previous source-bound compilation archives remain unchanged.

The task-installed MetalToolchain is verified as build `27A266a`, identifier
`com.apple.dt.toolchain.Metal.32023.921.5`, and removed with exit zero.
The signed final package then observes all twenty-two runtime graphs available
and passes actual SDR/HEVC/Auto delivery using its packaged shaders. A
development Metal compiler is therefore not required by those actual runtime
checks. This does not imply a notarized distribution installation.

## 26. Final signed package and normal-API source requalification

Final release source `1bf84609cd0e334933f0a8c568d5a119b89d8549` builds
in 12 minutes 55 seconds. Signed daemon SHA-256 is
`d7e06d95b1da490f497a92fd50db38d5c04d3683e7851caa733cb52b30db5a15`;
its matching arm64 dSYM UUID is `68d0c6c2-7ce4-3e95-b23b-03f2c415ef80`.
Jellyfin FFmpeg `c8c4b525…`, FFprobe `e81fff40…` and parser WASM `36f7e029…`
remain unchanged. Package source/hash/signature/entitlement/UUID validation
passes. This is an ad hoc signed private package, not notarized distribution.

With the temporary Metal compiler removed, normal source scanning and held
source authority produce all twenty-two runtime observations, normalized
1440p SDR, CPU/native forty-eight-frame pixel observations, captured-session
switch semantics, seek/resume and cancellation. Finite SDR HEVC and unbound
Auto on/off deliver actual AVC/HEVC initialization, codec-bearing master
playlists and forty-eight decoded frames. Separate HDR checks deliver native
HDR10-to-SDR and Main10 PQ/BT.2020 HEVC. Strict P5 delivers all twenty-four
analytic frames through the captured strict `vt_dovi_metal` identity. These
are bounded final-source rechecks; the earlier 120-second demand receipt
remains separately bound to `a2ee4a221`.

Normal AC-4 acceptance on `7ecf13dd1` now reads the first six-channel AC-4
track correctly and passes held-source attestation (miss 1, hit 1, refused 0).
Explicit software encoding delivers real AVC video plus stereo 48 kHz AAC
with monotonic A/V PTS, three objects representing 6.006 media seconds during
the five-second bounded timeline, and cancellation. The first software
control failed because its external oracle expected `sample_rate` on each
FFprobe frame; that field is absent. The corrected oracle checks actual
stream rate, frame channels/sample count and timestamps. Both are retained.

The public caption-bearing sample also exposes a negative result: incumbent
CPU processing with VideoToolbox encoding fails in its A/53 SEI insertion
path. This is not a successful native-processing result or a new native-route
fallback. The predeclared caption-bearing file-VOD limitation remains; no
blanket caption stripping, weakened source attestation or speculative parser
patch was introduced. The public sample and derived media are not retained.

[Final normal-API receipts and package verification](evidence/macos-video-20261008/normal-api-final-evidence.tar.gz)
contains thirty-three hash-verified files, 170,757 compressed bytes, SHA-256
`318f1b851d46bf9afdf84495a090f9d07325aff437321700cb59d6879a687604`.
Original source/driver hashes, bounded errors, decoded observations and
normalized executed drivers are included. Credentials, private sessions,
configurations, full daemon logs, binaries and media are excluded.

## 27. Final Live source-change and EOF acceptance

The signed `1bf84609` package passes both affected Live lifecycle checks.
EOF records the expected terminal `stream_failed` counter increment, ends
normal resume, and an explicit fresh request decodes 144 progressive frames
at 12 fps. The controlled format transition records exactly one
`source_format_changed` counter increment after 13.39 seconds, ends the old
resume capability, and an explicit fresh request observes 416 × 234 at
15 fps and decodes 144 progressive frames at that cadence. The ordinary
twelve-case frame/field matrix remains separately source-bound to `a2ee`.

The final receipt links the previously failed transition, accepted repair
commit `4be9603bc`, reviewed file blob and original transition manifest.
Actual argv retains software decode, native upload/scale, VideoToolbox
encoding and existing scoped Live `-a53cc 0`. The result confirms existing
owner cleanup now settles the diagnostic before terminal classification
and invalidates stale source facts; no new automatic retry is asserted.

[Final Live lifecycle receipt](evidence/macos-video-20261008/live-api-final-evidence.tar.gz)
contains one verified sanitized file, 3,102 compressed bytes, SHA-256
`7efe0192f6343b8f7f79146221a2a530bd37a7d59fbca0f3d68c4f7547f1c025`.
Private raw receipt/driver/log hashes are linked; media, capabilities,
LAN identifiers, configuration and credentials are excluded. All task-owned
daemons and tuner containers have settled; no owned listener remains on
ports 80 or 5004.

## 28. Startup sampling, admission and completed demand soak

The signed final `1bf84609` package completes thirty CPU and thirty native
starts, alternating order and using distinct normally scanned source identities
and uncached candidate recipes. Timers start before normal POST creation,
including source-fact/catalog/decision work, and end on fetched first media
bytes. Prior daemon identity initialization, settings and library scan are
excluded. Polling is 0.5 seconds; these are HTTP-availability upper bounds,
not cold-disk, click-to-play or presented-frame measurements.

| Route | Samples | Median seconds | Nearest-rank p95 seconds |
|---|---|---|---|
| CPU baseline | 30 | 1.225315 | 1.242879 |
| Native candidate | 30 | 1.206918 | 1.216253 |

The candidate satisfies the predefined p95 allowance: baseline plus the
larger of 100 milliseconds or 10%, yielding 1.367167 seconds. These close
upper bounds do not establish a measurable startup speedup. All starts
remain under the separate 30-second delivery deadline.

Normal admission settings change from two hardware/eight software slots to
four hardware/thirty-two software slots for the concurrency controls. One,
two and four distinct uncached sessions all admit and publish without errors;
the four-session first-media bounds span 2.732–4.252 seconds. This is short
admission/publication evidence, not advertised realtime encoder capacity.

The original combined driver then fails before its soak: it reuses a global
control sequence across four new sessions, and the existing owner correctly
returns `409 stale_control`. The original failed receipt is preserved. A
separate soak-only continuation uses independent client identities and
sequences, with all four first sequence-1 exchanges accepted before starting
the monotonic 1,800-second clock. No application patch or generic retry is
introduced, and the valid sixty starts/admission controls are not rerun.

The continuation completes 1,800.051757 seconds across four independent
sessions, fetching 1,096 objects and 54,458,496 bytes. Per-session controls
and producer-state checks record no failure. Shutdown exits zero and no
matching task-owned daemon/tool process remains. All effort-owned compiler,
archive, cleanup and competing experiment jobs are paused during measurement.
After materialization these are repeated cached-object demand/control checks;
they do not prove sustained encoder concurrency, actual client presentation,
unmeasured process memory behavior, energy or physical copy counts.

[Startup and final soak evidence](evidence/macos-video-20261008/timing-soak-final-evidence.tar.gz)
contains six verified files, 140,893 compressed bytes, SHA-256
`48b843d948a755b0e8a5aa681aeebd76bcc694cb76f4779ba120ab3970d835ae`.
The successful continuation links original failed receipt SHA-256
`19f4acfcd2e7b3621ab4726535f08eb095f7550dbcef9502efb733f18666c953`.
Source/binary identities, original driver hashes and normalized executed
copies are retained. Media, credentials, private session URLs/configuration,
full daemon logs and binaries are excluded.

## 29. Required follow-up mechanisms — 2026-10-08

These controls support the active GPU-subtitle, non-Mac P5 and caption-bearing
VOD work. They are not final shipping-package or normal-API qualification.
The [status ledger](MACOS-VIDEO-PROCESSING-STATUS.md) names the remaining work.

| Control | Observed result | Limit |
|---|---|---|
| Animated ordered ASS | 24 frames, exact Y/UV and PTS, including blank transitions | Tiny analytical input; direct graph only |
| Colored bitmap odd boundary | 24 frames, exact Y/UV and PTS after matching CPU boundary alpha arithmetic | Generic BGRA overlay is outside this new mode |
| ASS clipping at all edges/corners | 24 frames, exact Y/UV and PTS, including negative coordinates and blank frames | Same staged Fontconfig provider; no performance claim |
| Caption-preserving H.264 VT | Seven direct controls pass; caption-positive HEVC and MPEG-2 each retain 48 caption records | HEVC output still has no A53; actual API acceptance remains separate |
| Original finite-VOD clock | Two decoded source frames near PTS 66,272 yield 17 scheduled observations of one picture | Confirms source-origin defect; output presence is not preservation |
| Corrected seek/A-V clock | 17 observed video frames and 144 actual audio frames match normalized reference in timing/checksums | Direct seek3/preroll2 plus175ms correction; final API still required |
| Intel VAAPI strict P5 | All 28 diagnostic controls pass; fresh/variable raw hardware/software pixels equal | Ubuntu diagnostic, 320×180→160×90; no Bookworm shipping, 4K, other backend or performance claim |

The subtitle tolerance was declared before comparison: maximum absolute
luma/chroma error two, with cue-free frames byte-identical. None of these
passing cases required tolerance changes. Rejected BGRA/affine alternatives,
initial frame-pool and class-layout errors, animation timestamp rounding and
bitmap boundary errors remain recorded in the ledger. The three passing
compositor receipts bind different intermediate binaries; no result is
silently attributed to a later final package.

The caption parser repair corrects logical RBSP payload counting through
escaped EBSP and preserves the existing SEI prefix. Its direct-control binary
is `9c9e91054999531e7543f603ca4d4a6db32a6399f92f9b283b245ae0d104c3ea`.
The VOD-clock controls use
`10aedbbbb73c6c4aa78a090c001620dc84f5d94c810ca5b207d1ba9aee6acd37`.
The [caption record](VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md) explains both
independent causes, the initially discarded audio, and the reference's
corrected timestamp precision. The actual API's padding-heavy output and
forced shutdown remain failures until corrected-candidate delivery proves
otherwise. Neither frame count nor a successful child exit closes that work.

The Intel diagnostic binary is
`8baa3b11d77f469f16169de4b3d2992568c05c419a41666948507c1ed70e4d14`.
It observes selected opaque VAAPI frames, mandatory download and effective
current-frame Dolby metadata through the strict CPU renderer. Correct
canonical Jellyfin driver lookup was necessary; ordinary libva overrides
were insufficient. Changed deployment state and stale transfer preflight
were refused before GPU access. All owned work was reaped after the window.

[Follow-up mechanism evidence](evidence/macos-video-20261008/followup-mechanism-evidence.tar.gz)
contains one curated JSON file, 3,824 compressed bytes, SHA-256
`f242819010450715562f1a5e6d03e81925d15a207cc8a1da5c6ad1163fdc368b`.
It retains exact tool/source hashes, private raw-receipt hashes, declared
thresholds, per-case outcomes and canonical observation hashes. Media,
caption text, credentials, private paths, session configuration, raw commands
and full daemon logs are excluded. The assembler's SHA-256 is
`d4455b56fb904a63ad805dc07d9d2b0389e836feb9b9b490a96f4a62950b19ee`;
its raw inputs remain in the owned evidence workspace for final review.

## 30. Caption-bearing normal API delivery — 2026-10-08

The combined source `1f154545619d49e81f994983ea58dd6b6c160f5a`, signed
debug daemon `ddb72eb4a83cc412f885e2a958bd9aa7908c74db0f0d1bbc325223c4a9c54797`
and FFmpeg `10aedbbbb…` deliver the corrected finite-VOD graph through normal
file scanning, session creation, HTTP media and cancellation. Captured held-FD
commands select `h264_videotoolbox`, apply the held source offset to both
media inputs, and disable their extra automatic seek trimming. This is a
private correctness package, not a release or performance result.

| Input | Delivered frames | Caption evidence | Independent picture PSNR, minimum / mean |
|---|---:|---|---|
| Public reordered HEVC/AC-4 | 120 | Exact raw per-frame CC/PTS equality with the identical software graph; all 120 due 608 and 66 due valid 708 records retained in source order | 41.48 / 42.4773 |
| Original MPEG-2/A53 | 50 | All 48 ordered 608 records retained; exact software CC/PTS equality; initial-frame oracle corrected as described below | 37.86 / 38.4124 |
| Caption-free original | 50 | Zero caption records; exact software CC/PTS equality | 37.86 / 38.4124 |

The picture oracle was frozen before execution at minimum PSNR 24 and mean
30, with source-picture progression and packet/decoded-frame PTS checks.
It compares independently normalized source pictures; output frame count
alone is insufficient. Caption comparison retains raw tuple bytes and PTS.
No client caption-presentation claim follows from these server-side results.

The MPEG-2 run initially failed its source-time oracle. Format start is 1.4 s
and the first video picture starts 38.7 ms later. At 30000/1001 fps, the first
two pictures round to ticks 1 and 2. The pinned `fps` filter queues their
captions before cloning the first picture at output tick 0; its 608 FIFO
drains two records into that first output. The second record's source time,
72.0667 ms, exceeded the original two-frame allowance of 66.7333 ms.

The revised oracle recognizes only that exact initial assignment: output
tick 0, source frames 0/1, nearest first tick 1, consecutive source frame
timing and exactly two queued/drained ordered 608 records at 30000/1001.
The two-frame bound remains unchanged for every later record. Reassessment
uses the same hash-verified source, served media and software reference;
there was no second API or GPU run and no production change. The frozen
failure remains in the evidence. Existing first-picture padding does not
promise preservation of the original initial video presentation onset.

All session cancellations return 204, but all three debug daemons exceed
the 30-second shutdown settlement bound. Owned-PID stack samples identify
Tokio's blocking-pool shutdown waiting for startup `DecodeProbeIdentity`
WebAssembly compilation. Parser identity had already exceeded its deadline;
the worker continues compiling. This is separate from the completed
VideoToolbox producers. The owned processes were terminated and reaped.
The exact-source release-profile check in §31 distinguishes the debug compiler
cost from its observed release lifecycle; no watchdog, deadline increase or
parser-capability bypass was added.

[Caption API delivery evidence](evidence/macos-video-20261008/caption-api-delivery-evidence.tar.gz)
contains three sanitized JSON records, 2,674 compressed bytes, SHA-256
`e4cc44d9665c057e5b2696b396e3d37dea42e120100d052723fa9f2e3f54ea6a`.
They retain the original failures, reassessment, exact artifact/observer hashes
and qualification limits. The archive excludes media, caption text, private
paths and raw logs. Its assembler is
`a245edf2880a060ceccb800331eec0f1988434c07d7da07912d6a23091450131`;
all referenced raw receipt/media hashes were checked before assembly.

## 31. Release parser lifecycle and retained subtitle-driver failure — 2026-10-08

Exact source `1f154545619d49e81f994983ea58dd6b6c160f5a` builds with pinned
Rust 1.97.1 in the release profile. The first bounded window expired after
600 seconds; no source error was reported, and the exact unfinished crate
was not recorded. A separately authorized 1,800-second window completed in
792.69 seconds with two compiler jobs. Both receipts remain retained.

The immutable unsigned daemon is 168,070,376 bytes, SHA-256
`a1ba7bdfb904671a77afa465c90c77db81abc89e4838c5438047d664e4610507`.
Its arm64 UUID `D8A72604-6217-3E8D-A1F5-52E45FAEB4B6` matches the retained
debug symbols. The signed qualification daemon is
`42fdd61b5431fe657bfd1435497fc5dc8c11595b38cc385b86a8f3a4ac0d83cb`;
FFmpeg, FFprobe and sealed-parser bytes remain unchanged from §30. This is
ad hoc package qualification, not a notarized installation claim.

With the owned build prefix hidden, the first normal CPU subtitle API run
passes all 49 independent source-cue observations, has no startup parser
identity warning and shuts down with exit zero. Its overall result remains
**failed**: the first hold request returned 200, but the private driver then
sent `play` for resume, which protocol v1 rejects. The valid value is `active`.
The original receipt and executed driver are retained unchanged; the driver
correction changes no application behavior.

This result supports release parser initialization and clean shutdown for
that case. It does not turn the three debug caption shutdown failures into
passes, qualify GPU subtitles, or establish complete control lifecycle or
performance. Text/bitmap API work continues separately in the status ledger.

[Release parser lifecycle evidence](evidence/macos-video-20261008/release-parser-lifecycle-evidence.tar.gz)
contains one curated JSON record, 1,359 compressed bytes, SHA-256
`bf81a44ed7b08a1e0a0527b148ffee91cd0688c85224dba931bb4c0147d965df`.
The assembler verifies the unsigned daemon and all symbol hashes, retains
the original receipt hashes and failing overall result, and excludes runtime
directories, media, credentials, paths and commands. Its SHA-256 is
`6896e303ace63b5d8c65a0a22f74b151283f2368dc7a75d442a5d677c06a7f03`.

## 32. GPU bitmap API delivery and local text authority — 2026-10-08

The same signed release source and package as §31 now pass normal bitmap
subtitle delivery with the owned build prefix hidden. The actual producer
selects `overlay_videotoolbox=bitmap=1` and a two-second containing entry for
a requested 3.5-second start. All 25 served frames match the independent
source-event model described below. Hold, resume and seek-back-to-zero
requests return 200; DELETE returns 204, and the daemon shuts down with exit
zero with parser identity available. The build prefix is restored and all
owned media processes are reaped. Control acceptance is not proof of a
client-presented frame or rendered seek-back accuracy.

The first GPU and incumbent CPU cases both failed the original raw-interval
oracle on the frame at 3.750 s. Source packets and decoded sidecar timestamps
remain 0.021, 0.521, 3.271 and 3.771 s; no 21 ms origin subtraction occurs.
Tracing the pinned `sub2video` and frame scheduler establishes the exact
assignment:

- With pass-through EOF behavior the secondary input has `sync=0`, so the
  main 1/12 clock determines nearest timestamp conversion.
- A blank heartbeat at 3.270999 s precedes the active subtitle at 3.271 s.
  Both become tick 39. The scheduler consumes one queued next event per
  input and emits the main frame on the first tie, leaving tick 39 blank.
  The active event takes effect before tick 40, at 3.333333 s.
- The clear event at 3.771 s becomes tick 45 and applies to the main frame
  at 3.750 s.

The deterministic reassessment passes all 25 retained frames in both original
CPU and GPU outputs. Their failed receipts remain unchanged. This is an
oracle correction derived from exact event ordering, with no application
change or tolerance increase. A separate corrected-driver GPU run then
completed the controls that those failed runs had not reached. DELETE's 204
is established by the executed driver's assertion and passing path, not an
invented receipt field.

The text case exposed a different, real integration defect: local resolution
received a font digest only from shared-source evidence, while local engine
capture happened after planning. Repair `b7d21934b` moves the same capture
before resolution and retains that engine through encoding. Matching, absent
and mismatched font regression sources compile. The pinned all-target check
and normal hook pass without unit execution; updated text runtime acceptance
is still outstanding. The bitmap runtime results above use `1f1545456` and
must not be attributed to the later text repair.

[Bitmap API and text authority evidence](evidence/macos-video-20261008/subtitle-bitmap-api-evidence.tar.gz)
contains one curated JSON record, 3,844 compressed bytes, SHA-256
`2a478286f56658f4916757d0092a5a01528924fb159c9eb6b56771ed89b4ec29`.
All 33 raw allowlist entries were independently checked for hash and size
before archiving; the archive was reopened and compared byte-for-byte. It
includes the per-frame expectations, original failures, exact artifact and
driver identities, and explicitly separate compile-only evidence. It excludes
media, paths, raw commands, session identifiers, credentials and runtime state.
The curator is
`e04fb3b735a5c347b00ededf549f537eb2a9e41309841ecd1a634a79dfcd569d`.


## 33. Linux SDK generation and release integration — 2026-10-08

Static staging and actual generation are distinct. The first 75-role static
stage required a source-format correction: authenticated Debian format 1.0
uses an original archive plus a declared `.diff.gz`, whereas 3.0 (quilt) uses
an original archive plus a Debian archive. The helper now checks the genuine
DSC source/version, exact SHA-256 member set and sizes for each format. The
original refusal and a blank-continuation parser failure remain retained.

The actual generator used committed source
`070ac88142ffd2c473e2149dea34371d272e9c1b` and static manifest
`5898c47a16ad92cf71ac71354b38b269eb90c7556eec7f981ac9f4e81e4a2316`.
Its source-only helper/lock archive is
`8823e91a2d40213898db293674c8d2c7be7e43e662f6df876b338e7ab2b16ae7`.
Fresh preflight observed Debian 12 amd64, GCC 12.2 and Python 3.11.2; input
hashes passed. The isolated container had two CPUs, 2 GiB, no swap, no GPU
and no network during generation. No repository metadata or credentials were
transferred. The attempt failed after 21.60 seconds, without an OOM or timeout;
its container stopped and its original network configuration was restored.

The failure was an omitted upstream FFTW metadata target. Float configure
produced `fftw.pc` with `Libs: -lfftw3f`. The authentic Makefile declares
`PREC_SUFFIX=f`, `pkgconfig_DATA=fftw3f.pc`, and copies `fftw.pc` through the
`fftw3f.pc` target. Configure alone therefore cannot satisfy that selector.

| Evidence | SHA-256 |
|---|---|
| Authenticated FFTW source archive | `5630c24cdeb33b131612f7eb4b1a9934234754f9f388ff8617458d0be6f239a1` |
| Original `Makefile.am` | `0f4b72d870b6f532de6ca06ae1a97a8be2feec50ac37afd7c4e0e899e02875e4` |
| Actual generated Makefile | `d1ea4e50ecdca5be544355581563d4e71b183f36dc8cf64f152061b2fb90e1d4` |
| Actual float `fftw.pc` | `6e8b35f3d26ee0e7f15d9a17d85636e3106ee723e0213b7f6067eebec24f17e6` |

The repair adds the genuine `make -j{jobs} fftw3f.pc` step. Float options,
expected installed module and missing-output refusal remain unchanged.
Fresh local static staging passes in 6.59 seconds with approximately 764 MiB
peak RSS. The new role lock is
`dd3309c482684114a69a788f104f69ae49a527ef78efa59e9ddd4a87a18d76bb`;
the new static manifest is
`68e6a4b63a4e33328e48f4127a298ca5a023800681b2041eca1417ac70be6f2b`.
It retains 25 upstream dependency sources, 49 distribution roles, one pinned
Meson provider and all 57 Jellyfin configure tokens.

The corrected retry uses committed source
`c4beb5b5f1b26fdbb865ffc4ef47e26053fece97`. Its source-only archive is
`9cc24a0df4511a706caf19335aa4154add356453b08d7ce17233ccc0a655e221`.
All 6,596 input-file hashes verify before execution. The same isolated
Bookworm container runs offline with the same two-CPU/2-GiB bound. FFTW's
actual upstream metadata target succeeds, completing the AMF, dav1d,
FDK-AAC, FFNVCODEC and FFTW generation roles. The attempt then fails after
21.91 seconds while configuring FreeType 2.14.3, without an OOM or deadline.
The owned container stops and its network configuration is restored; both
failed attempts remain retained.

FreeType's genuine `check_out_submodule` rule invokes `git submodule update`
for `subprojects/dlg`. The archive-only source lacks that pinned submodule,
so configure cannot complete. This is a source-closure omission, not a
compiler-resource failure. The repair must include the authentic parent
revision's gitlink source, license and source offer. Fake Git metadata,
disabling the feature or weakening output checks would conceal the omission.
The source repair now supplies six required submodules: FreeType's `dlg`
and libplacebo's GLAD, Jinja, MarkupSafe, fast_float and Vulkan-Headers.
Each is bound to the actual parent gitlink and `.gitmodules` declaration,
its downloaded archive and authentic license. Composition refuses to replace
existing parent source bytes. Optional demo/test submodules are not consumed.
The same source audit finds eleven dependency roles whose accepted local
Git-archive hashes are paired with repository URLs in the cold-fetch path.
Those URLs cannot supply the declared archive bytes. The repair must bind
actual downloadable public archives of the same pinned commits, with observed
hashes and normalized source-tree equivalence; cached files cannot establish
that a fresh release build works. Prior accepted archive evidence remains
historical. All eleven replacement archives now have observed hashes and
exact normalized-tree equivalence, including contents, executability and
symlink targets. The equivalence receipt is
`5c3c0ff7e89df2af4d13b42563af43b095b9a15afd48a9a764c1b4470158b8c3`;
the required-submodule authority receipt is
`d7c9b6b9d4aac59b44f3939b290f3ab4231797f1cac8435e396737d694d0aa2f`.
The repair is committed as `50784a2b6a803b27403f545117bdd053fbec76d0`.
Its normal hook passes in 73.04 seconds. Fresh static staging passes in
7.25 seconds with peak RSS 800,325,632 bytes and manifest
`ca95708df14a920a004a41f68d7c55e8c7bc378ed4820787f84c450b116d5188`.
The exact source-only retry verifies all 6,620 input-file hashes before
generation. FreeType autogen and configure now pass, proving the required
submodule repair reaches the actual generator. The collector then refuses
the absent `builds/unix/freetype2.pc` after 23.60 seconds. There is no OOM or
deadline; the isolated container stops and its network configuration is
restored. The repair appends `make -j{jobs} builds/unix/freetype2.pc` after
the unchanged configure step. The pinned `unix-def.in` defines that genuine
target; the actual configured makefile resolves `OBJ_BUILD` to `builds/unix`.
The producer audit finds no further concrete omission of an upstream make
target in the remaining generator recipes. Actual post-repair generation
remains pending; no generated metadata is substituted by hand. Linking and runtime
qualification remain open.

| Evidence for the source-closure retry | SHA-256 |
|---|---|
| Static staging receipt | `1db36bd6e28fa902e16a99c4a36076a744a770010464c76cf55886771ae4ea2e` |
| Source-only helper/lock archive | `f0c20a8880a8957bd2c222eb82c27da1b2eb2eede1308f1a9c5680e0d1522be0` |
| Actual generator terminal receipt | `d11a6a76e80532415d63ec0c297f3ae8fab3eb9c379cd9cab7c4b07237352798` |
| FreeType upstream producer evidence | `9e0935266890627002b1150471205e22f79c4b7378c61cb43a39b69badfdad4c` |

The shipping source also closes a separate integration gap: published and CI
smoke images bypassed the local `make docker` seam. Both now have a shared
Bookworm package-export contract and consume its audited output; the renderer
selects the architecture-specific runtime while excluding build-tool stages.
The assets-only publication path does not require daemon Rust compilation.
No successful export, image build or physical-backend qualification is claimed
from source or syntax checks alone.
