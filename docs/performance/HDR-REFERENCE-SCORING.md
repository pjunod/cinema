# HDR reference scoring — compare grades, not labels

**Status:** open — offline M1 scoring path; corpus and physical acceptance owed.

Companion to [codec qualification](../streaming/CODEC-AND-GPU-QUALIFICATION.md)
§3.6 and [benchmarking](../BENCHMARKING.md). This describes the independent
offline scorer in [`scripts/codec-hdr-score`](../../scripts/codec-hdr-score),
not a server filter graph, a GPU qualification or a replacement for the
incumbent SDR [`scripts/bench`](../../scripts/bench).

## PQ metrics require a matched decoded pair

```bash
python3 scripts/codec-hdr-score \
  --reference /owned/reference.mkv --reference-sha256 REF_SHA256 \
  --distorted /owned/output.mkv --distorted-sha256 OUTPUT_SHA256 \
  --ffmpeg /absolute/regular/ffmpeg --ffprobe /absolute/regular/ffprobe \
  --duration 2 --shadow-code 80 --highlight-code 700
```

Inputs must be hash-pinned regular local files without symlinked parents.
Resolve a tool installation's symlink before naming the executable; the
receipt records the actual binary hash and version, not its installation alias.
This POSIX-only execution path has no network protocol, shell interpolation,
production process or post-fork callback. Each fresh tool process gets a
90-second CPU limit and wall deadline and an 8 MiB per-file output limit.
The caller bounds duration to 30 seconds; decode accepts at most 600 frames
and dimensions at most 3840 × 2160. Metric/census files above 1 MiB are refused.

Every decoded frame must be limited-range ten-bit `yuv420p10le`, BT.2020NC,
BT.2020 primaries and ST2084/PQ. Container facts cannot contradict decoded
facts. Reference/output dimensions and strictly increasing timestamp grids
must match exactly; no scaling, alignment inference or repeated final frame
repairs an invalid pair. PSNR and SSIM each run a separate FFmpeg decode and
must attribute one result to every validated frame. A replaced input refuses
the result rather than attributing numbers to its earlier hash.

**How to read it:** PSNR and SSIM are measurements of PQ **encoded code
values**, not luminance in nits or an HDR-trained perceptual score. Infinite
PSNR is represented as `"identical"`, never invalid JSON infinity. The census
records each frame's luma minimum/maximum and fractions at or below the chosen
shadow code and at or above the chosen highlight code. FFmpeg's printed mean
has finite decimal precision, so fractions are estimates, not exact pixel
counts. Thresholds must be ordered integers within limited-range codes
64..940. They are not automatically inferred from MaxCLL or a film's artistic
intent. Compare both files' per-frame values; a high average can conceal a
changed shadow floor or clipped highlight.

Mastering display and content-light metadata are recorded at stream and frame
level and must match between reference/output. Both absent is reported as
absent, not invented metadata preservation. A DOVI configuration record or
decoded RPU/metadata refuses the pair: a genuine Profile 5 input first needs
an independently proved reshape and output-grade contract. HLG, SDR and
eight-bit inputs refuse this PQ path. There is no automatic transfer relabel.

## The authored reference is separate from every incumbent fixture

```bash
python3 scripts/codec-hdr-score generate-reference \
  --ffmpeg /absolute/regular/ffmpeg --destination /owned/new-reference.mkv
```

This authors a 64 × 64, four-frame/s, two-second neutral ST2084 test signal
using the inverse EOTF constants and a 0..1000 cd/m² ramp. Neutral R′=G′=B′
maps to nonlinear limited-range Y′; chroma is neutral. A lossless x265 encode
retains its decoded grade. The JSON records the exact graph, argv, tool hash,
output hash and authored-domain facts. The destination must not exist.

This is an actual generated diagnostic, not a captured HDR film or genuine
Dolby Vision sample. It neither changes nor qualifies the incumbent tagged
`testsrc2` HDR fixture. A mock or authored ramp cannot close the genuine-media
classes, production bitmap burn or captured-session matrix in M1.

## SDR VMAF requires a separate, pinned grade receipt

```bash
python3 scripts/codec-hdr-score grade-sdr \
  --source /owned/pq.mkv --source-sha256 SOURCE_SHA256 \
  --ffmpeg /absolute/regular/grade-ffmpeg \
  --ffprobe /absolute/regular/grade-ffprobe \
  --destination /owned/new-bt709-reference.mkv --duration 2.0

python3 scripts/codec-hdr-score score-sdr \
  --source /owned/pq.mkv --source-sha256 SOURCE_SHA256 \
  --reference /owned/bt709-reference.mkv \
  --distorted /owned/bt709-output.mkv --distorted-sha256 OUTPUT_SHA256 \
  --receipt /owned/grade-receipt.json --receipt-sha256 RECEIPT_SHA256 \
  --ffmpeg /absolute/regular/vmaf-ffmpeg --ffprobe /absolute/regular/ffprobe
```

Retain the grader's JSON separately and pin its SHA256 from that execution's
evidence; do not write a receipt merely to authorize already tagged bytes.
The fixed independent grade is CPU `zscale` to linear RGB at 100-nit reference
white, BT.709 primaries, Hable tone mapping with desaturation off and an
explicit 1000-nit input-peak assumption, then limited-range BT.709 eight-bit
output. The lossless x264 reference deliberately removes HDR frame side data.
The assumption is a declared reference choice, not a measured source peak or
proof of correct artistic grading. Physical HDR/SDR A/B remains required.
Missing `zscale`, encoder or model capability fails the actual execution; a
filter list is not a passing behavior probe.

The consumer verifies the exact grade graph/white/peak/tool provenance,
receipt hash, parent-source hash and decoded parent facts. Reference and
distorted output must both decode as BT.709 eight-bit limited-range with the
same dimensions/grid as the source, without residual HDR light/mastering or
DV metadata. Only then does the separate scorer execute `vmaf_v0.6.1` on
every frame. The reported domain is **SDR BT709 VMAF**, never HDR VMAF.
The parent-dimension check is independent of reference/output agreement:
resizing both SDR files together still refuses before model execution.
The receipt is externally pinned provenance, not a signature or a proof that
an arbitrary caller executed its contents; retain the original execution
log/tool/image identities for independent acceptance.

## Current evidence is diagnostic, not M1 closure

On 2026-10-01 an eight-frame authored PQ reference was actually generated
with local FFmpeg 9.0.1. A distinct CRF32 output scored 55.17 dB PSNR and
0.998768 SSIM per frame; luma maxima changed 722→723. The original FFV1
attempt lost explicit decoded transfer/primaries and was correctly refused.

A bounded owned network-none container on the accepted `4f243a01` artifact
ran the independent grade using Jellyfin FFmpeg 8.1.3, 1 CPU / 1 GiB, then
was removed after retaining both log channels. The resulting BT.709 reference
decoded to the same eight-frame grid. Local FFmpeg 9.0.1 actually executed
libvmaf and reported 97.428132 for each self-reference frame: this tiny-signal
model diagnostic is not a film-quality threshold or a GPU ranking.

A bounded read-only private-library header census also found a genuine HEVC
Profile 5, RPU-present, BL-present / EL-absent candidate. Its private path,
17,950,785,386-byte stat identity and first-MiB hash are retained privately;
the prefix hash is **not** a whole-source hash. Full hashing/acquisition and
per-frame RPU proof remain open, rather than reading the large source under
concurrent fuzz I/O or exporting it broadly.

The currently observed M4 machine is a MacBook Air; the local machine is an
M3 Max. Those observations do not establish that the user's M4 MacBook Pro
is unavailable. VideoToolbox compiled support, selected graph eligibility
and physical-client acceptance remain separate facts.

## What this does not qualify

No HDR-trained model has been identified or executed. No physical A/B,
production session capture, GPU graph comparison, new codec default,
Developer switch, client deployment or clock/performance qualification has
occurred. The incumbent SDR scorer and fixture recipes remain byte-identical.
M1 and the overall codec/GPU plan stay open.

The three focused synthetic methods in
[`test_codec_hdr_score.py`](../../tests/operations/test_codec_hdr_score.py)
each passed once during development. Their tracked local-pass records retain
uncommitted snapshot hashes and tool IDs. The earliest retained `88d450…`
snapshot is **after** the temporary-path fixture fix; the first invocation's
pre-fix whole-file hash is unavailable. It is not attributed as the exact
first execution source. The first method's unchanged assertions are offered
as individually reconstructed evidence, not whole-file equivalence.
Because compaction did not
retain the verbatim tool output, those records explicitly reconstruct
individual results; the first invocation also contained a different method's
fixture error. They require independent authenticated admissibility review,
not an assertion that the entire invocation was green or a current-tree run.
No successful method is repeated to manufacture a cleaner receipt.

Review43 found the missing parent-dimension check at the original candidate.
The new
[`test_codec_hdr_parent_geometry.py`](../../tests/operations/test_codec_hdr_parent_geometry.py)
consumer case failed against that source for both width-only and height-only
joint resizes, then passed once after the check was added before libvmaf.
That correction remains within the same sole review; earlier successes are
retained at their original source hashes, not repeated or relabeled.

## October 8 continuation — isolate the sharp edge before changing policy

**Decision: retain the production policy.** The bounded calibration is complete,
but the sharp-edge image criterion still fails. Both tested alternatives have
measured drawbacks below. No image threshold, encoder flag or feature gate
changed. The earlier continuous-ramp signal proof remains a narrower pass.

The [video-quality programme](VIDEO-QUALITY-PROGRAM.md) asks for autonomous
captured-image calibration. Its physical-display non-goal remains separate
from S-11's broader device and artist-grade acceptance. The new
[comparison receipts](../evidence/video-quality-2026-10-08/hdr/comparison-summary.json)
are finite measurements on lab6's shipping image, not a claim that every HDR
source or client is qualified.

The original three-panel failure is reproducible. At the current image,
its baseline encodes to the same SHA-256 as the retained older-image capture:
maximum luma error **35**, mean **0.368015** ten-bit codes. The quality bars
remain mean below 4 and maximum below 32. A separate decoder gives the exact
same raw frame. Disabling HEVC in-loop filtering worsens the maximum to 39,
so removing that filtering would compound the error.

The [stage control](../evidence/video-quality-2026-10-08/hdr/stage-control-recovered-reference.json)
compares every Y/U/V sample after source decode, software scale and P010
conversion, and GPU upload/download. Each result is byte-identical to the
original analytical reference. These measurements localize the damage to
lossy encoding/reconstruction; tags, packing, scaling and upload are not the
cause. The maximum lies at a sharp panel transition. This is an encoder
quality/rate-control decision, not a reason for another playback retry or
for weakening the image threshold.

A less restrictive `-qmax 24` is byte-identical to the baseline. The explicit
`-qmax 18` candidate passes the same sharp bar at both the original 20 Mbps
diagnostic rate and the actual 1080p 8 Mbps rung: maximum **12**, mean
**0.214770**. At 8 Mbps its synthetic file is 58,080 bytes versus 68,974.
The [actual-rung pair](../evidence/video-quality-2026-10-08/hdr/sharp-8m-comparison.json)
retains metadata, exact timestamps, actual hvcC, source/output hashes,
commands and confirmed container/watcher cleanup. These synthetic savings
are not an expected film saving.

Two disjoint 48-frame windows from one private, plain-PQ 3840×2160 title
were measured at 8 Mbps with the same 12 Mbps maximum and 16 Mbit buffer.
Only anonymous numeric receipts leave the host. Mean PQ-code PSNR improves
by 0.163 and 0.157 dB, while bytes increase **14.45% and 15.96%**. Mean SSIM
improves, but worst-frame SSIM decreases by 0.000017 and 0.000084. The
highest one-second packet rates are 8.19 and 8.82 Mbps. A separately authored
colored moving/noise source improves worst-frame PSNR from 50.30 to 52.20 dB
for 4.36% more bytes, with a 7.90 Mbps peak. All compared outputs decode to
the same 48-frame rational grid and PQ/Main10 grade. Code-domain metrics are
not an HDR perceptual score; two-second encode times on a two-CPU container
do not establish sustained production throughput.

The first real-title diagnostic used a 3840×2080 source forced to 1920×1080.
That source does not fit the production aspect-preserving HDR rung, so its
20 Mbps result is retained as a diagnostic only. The two later windows use
a native 16:9 source and the actual rung budget. A missing remote raw frame
also refused the first stage-control attempt before a container started;
its failure remains recorded, followed by a hash-pinned analytical reference
restoration. Neither refusal was relabeled as a pass.

### Both candidate policies were rejected

The client bitrate ceiling can reduce the HDR encoder budget below the normal
8 Mbps rung. At a 2 Mbps target, 3 Mbps maximum and 4 Mbit buffer, `qmax 18`
emits **11,318,880 packet bits over 2.002 seconds**. That exceeds even a
full initial buffer plus maximum-rate allowance of **10,006,000 bits**. Its
one-second peak is 5.75 Mbps; the baseline peaks at 1.83 Mbps. The
[pressure receipt](../evidence/video-quality-2026-10-08/hdr/pressure2m-result.json)
rejects a universal upper quantizer bound. Adding an arbitrary bitrate cutoff would
hide the same unmeasured risk on harder content, so no such cutoff is added.

A final, distinct candidate changes hardware effort with `compression_level 1`
and leaves quantizers free to satisfy bitrate control. FFmpeg documents
[VAAPI compression level](https://ffmpeg.org/ffmpeg-codecs.html#VAAPI-encoders)
as a speed/quality tradeoff. This candidate passes the sharp bar (maximum 12,
mean 0.284579) and keeps the 2 Mbps pressure peak at 2.25 Mbps. Its result is
still mixed: the pressure sequence's mean PSNR improves 0.308 dB, but its
worst frame falls from 47.53 to 45.98 dB. On the two real windows, mean PSNR
changes by −0.006 and −0.133 dB, while mean SSIM improves. Bytes change by
+3.82% and −4.22%. This does not justify calling the alternative a consistent
quality improvement or changing the production default.

The [qualification decision](../evidence/video-quality-2026-10-08/hdr/qualification-summary.json)
therefore retains bitrate-first production behavior. **Sharp-edge acceptance
remains failed.** The experiments identified lossy encoder rate allocation and
reconstruction as the relevant cause and rejected an apparent fix that breaks
the network contract. That is a completed calibration decision, not completed
broader image/client qualification. No new retry, watchdog, switch or
per-title exception is introduced. Successful existing baselines were retained;
only the new candidate ran on those same source conditions.

### Chromatic SDR captures extend the neutral screen

The [supplemental generator](../evidence/video-quality-2026-10-08/hdr/chromatic-calibration.py)
reuses the existing production-bound tone-map harness with eight analytical
BT.2020 swatches, including an explicitly synthetic warm proxy, at three
exposures. Four PQ/HLG cases and three historical/current/no-dither variants
produce twelve matched eight-frame BT.709 outputs on the shipping Jellyfin
build. The [report](../evidence/video-quality-2026-10-08/hdr/chromatic/report.json)
retains commands, tool/source hashes, metadata, frame correspondence and
per-frame luma measurements.

The agent inspected all four contact sheets:
[PQ without MaxCLL](../evidence/video-quality-2026-10-08/hdr/chromatic/pq-absent-contact.png),
[PQ1000](../evidence/video-quality-2026-10-08/hdr/chromatic/pq-1000-contact.png),
[PQ4000](../evidence/video-quality-2026-10-08/hdr/chromatic/pq-4000-contact.png), and
[HLG](../evidence/video-quality-2026-10-08/hdr/chromatic/hlg-contact.png).
Rows show historical/current/no-dither; columns show 0.25/1/1.75 seconds.
Neutral patches stay neutral,
the warm proxy keeps its hue orientation, and no frame corruption or temporal
flicker is visible. Saturated BT.2020 primaries clip in every variant;
these deliberately out-of-gamut patches do not certify a perceptually
optimized gamut map. On PQ4000 the current graph's luma maximum is 236
versus the historical 254. Dithered PQ has one code above nominal 235;
the corresponding no-dither outputs peak at 235. This is disclosed rather
than called a strict legal-range clamp. The
[inspection record](../evidence/video-quality-2026-10-08/hdr/chromatic/inspection.json)
binds the actual PNG hashes and all four numerical ranges.

These captures isolate the tone-map helper, rather than the entire production
output contract. Their PQ1000/PQ4000 H.264 variants retain input Content-light
SEI. The actual production chain separately deletes mastering-display and
content-light frame metadata after tone mapping; that existing cleanup was
outside the standalone harness. Thus the pixel comparison and BT.709 color
tags are evidence, but these files must **not** be used as independently
qualified SDR grade references. No production metadata defect was found.

The first local attempt refused because that FFmpeg has no `zscale` filter;
the retained shipping-image run supplies the real execution evidence. No
new tone-map default follows. These are captured analytical images, not
real skin, an independent artistic reference or a physical display reading.

The [cleanup receipt](../evidence/video-quality-2026-10-08/hdr/remote-cleanup.json)
confirms removal of all 23 exact directories owned by this continuation after
inspection and receipt collection. No running container mounted them. The
borrowed original failure bundle remains with its existing owner and retention
policy. Private title bytes were never exported, and production media,
settings, queues and services were untouched.

A follow-up audit found the image-declared `/var/lib/plurx` volume survives
container removal without `-v`. The same cleanup receipt records all sixteen
volumes from these controls, matched by creation time to the reserved experiment
windows. Each was empty and unreferenced before exact deletion. Other owners’
volumes were excluded; no broad prune was used. Future isolated Docker captures
must remove their anonymous volumes with `docker rm -fv`.

## October 12 — enable adaptive quantization without preanalysis

**Decision: the sharp-edge candidate now passes the unchanged image bar.**
The [retained comparison summary](../evidence/video-quality-2026-10-12/hdr/qualification-summary.json)
is the source of the numbers below. Production integration and final validation
are tracked in [the programme ledger](VIDEO-QUALITY-STATUS.md).

The earlier description of `compression_level 1` as merely more encoder
"effort" concealed two independent controls. On the actual loaded Mesa
26.0.8 `radeonsi` driver, its value combines a balanced preset, preanalysis,
and variance-based adaptive quantization (VBAQ). The default disables both
preanalysis and VBAQ. The
[upstream quality mapping](https://gitlab.freedesktop.org/mesa/mesa/-/blob/mesa-26.0.8/src/gallium/frontends/va/encode.c)
and [bitfield layout](https://gitlab.freedesktop.org/mesa/mesa/-/blob/mesa-26.0.8/src/gallium/frontends/va/va_private.h)
make these controls separable. This is driver-specific semantics: `19`
must never be applied indiscriminately to every VAAPI implementation.

The actual FFmpeg verbose driver line and the loaded shared-library hash
identify bundled Mesa **26.0.8**. The system package inventory says 22.3.6,
but that is a different library. Driver selection, rather than an installed
package's version, is the relevant evidence.

| Sharp-panel control | Maximum / mean error, ten-bit codes | Outcome |
|---|---:|---|
| Production baseline | 35 / 0.368015 | Existing maximum bar fails |
| Block rate control | 35 / 0.368015 | No improvement |
| Balanced preset alone, quality 3 | 35 / 0.368015 | No improvement |
| Preanalysis alone, quality 9 | 35 / 0.368015 | No improvement |
| Balanced preset + VBAQ, quality 19 | 12 / 0.284579 | Mean below 4, maximum below 32 |
| Balanced + preanalysis + VBAQ, quality 27 | 12 / 0.284579 | Same sharp output as quality 19 |

Two early files named `balanced` used value 2 before inspecting the driver
ABI. That name was incorrect; the failed diagnostics remain in the bundle
and are explicitly excluded as proof of a balanced preset. Their output is
identical to the baseline. No failed result was deleted or relabeled.

These ablations isolate the sharp improvement to VBAQ. The separate
pressure experiment isolates the previous worst-frame degradation to the
preanalysis combination. Quality 19 keeps VBR, its target, its 1.5× maximum,
and its 2× buffer. It imposes no upper quantizer bound and does not preprocess
or recognize the test image.

At the same 2 Mbps pressure condition, the candidate emits 3,672,752 packet
bits in 2.002 seconds; the maximum-rate-plus-full-buffer allowance remains
10,006,000 bits. Its one-second peak is 1.917 Mbps. Mean/worst-frame PSNR
improves from 47.795/47.53 to 47.861/47.62 dB, and mean/worst SSIM also
improves. Quality 1 reproduces the older worst-frame PSNR regression to
45.98 dB; removing preanalysis avoids it.

Two source-local, disjoint 48-frame plain-PQ title windows retain native
16:9 geometry and the 8 Mbps rung. At 1800 seconds, candidate mean/worst
PSNR improves from 48.582/47.75 to 48.590/47.79 dB; mean/worst SSIM also
improves, for 2.86% more bytes. At 3600 seconds, worst PSNR improves from
49.64 to 49.89 dB and both SSIM measures improve. **Mean PSNR falls by
0.17646 dB while bytes fall by 8.51%.** This tradeoff is accepted explicitly;
it is not all-metric dominance or a claim that a code-domain score measures
HDR perception. Both windows stay inside the same bitrate envelope.

The first 3600-second attempt mistakenly retained the encoder's 1800-second
seek while moving the reference. Identical output hashes exposed the
mismatch. Its entire comparison is rejected and retained; only that invalid
window was repeated after aligning both seeks. Frame counts, color grades
and original timestamp grids are retained. The FFV1 Matroska reference has
a 1 ms clock; MP4 uses 1/24000. Metrics pair the same 48 decoded frame indices
at the declared 24000/1001 cadence; the Matroska rounding is at most 0.5 ms.
This is explicit index correspondence, not literal timestamp equality or
admission by the stricter standalone HDR scoring tool.

### Native fixtures retain their metadata repair provenance

A separate 24-second sharp-panel source supplies HEVC Main10 plus silent AAC
and two-second independent fMP4 HLS objects to the client lanes. The initial
direct-raw harness omitted source frame color properties. FFmpeg output
flags alone left its HEVC SPS primaries and transfer unspecified. This was
a fixture defect, not a production-graph defect: the earlier comparison
controls read fully tagged FFV1 sources and retain their decoded grades.

The repaired native fixture changes only SPS color metadata to the known
analytical source's BT.2020/PQ limited-range grade. The
[repair receipt](../evidence/video-quality-2026-10-12/hdr/native-metadata-repair.json.gz)
proves every coded-picture payload hash, the first decoded frame hash, and
all 576 frame timestamps are unchanged. Every repaired decoded frame then
reports BT.2020/PQ. Tier, level, quantization and bitrate are untouched.
Initial client decoding observations remain attached to their original
untagged assets; they do not become PQ-display acceptance retroactively.

The generator and the explicit metadata repair are both retained, including
the failing original output identities. Private title bytes remain on the
lab host. These bounded controls do not close physical colorimetry, artistic
grading, the whole genuine-media corpus, or the separately owned S-11/DV
programme.

The [cleanup receipt](../evidence/video-quality-2026-10-12/hdr/remote-cleanup.json)
confirms removal of all fourteen exact remote scratch roots after checking
container mount conflicts. Each bounded container used automatic removal
or exact `docker rm -fv`; no broad cleanup or production change occurred.
Public local client fixtures remain only until the parallel client lanes
finish, with final cleanup recorded in the programme ledger.
