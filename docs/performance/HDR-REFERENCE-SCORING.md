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
the original uncommitted test hashes and tool IDs. Because compaction did not
retain the verbatim tool output, those records explicitly reconstruct
individual results; the first invocation also contained a different method's
fixture error. They require independent authenticated admissibility review,
not an assertion that the entire invocation was green or a current-tree run.
No successful method is repeated to manufacture a cleaner receipt.
