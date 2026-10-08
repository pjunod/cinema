# Combined Dolby Vision processing — bounded source-to-HDR10 control

**Status:** open — combined synthetic mechanism reviewed; movie and production
integration remain in progress · **Updated:** 2026-10-08

The [implementation ledger](DV_HDR_PROCESSING_STATUS.md) tracks the product.
The [retained bundle](../../tools/dv_quality/backends/combined/README.md)
records the first combined decoder, reconstruction, timestamped intermediate
and HDR10 encoder execution. Its frozen README predates final review;
independent review approved the corrected bundle described here.

## 1. What actually ran

A real six-picture, B-reordered P7/FEL input passes through FFmpeg demux/split,
checked decoded BL/EL/fresh-RPU association, public libplacebo reconstruction,
RGB48 NUT muxing and an existing x265 Main10 encoder. Output is decoded again
for pixel, format and timing checks. These are synthetic 64×64 pictures.

Actual output preserves PTS 0/42/83/125/167/208 ms and stored 41 ms durations.
The rounding gaps do not prove continuous presentation coverage. The decoded
output is Main10, limited-range BT.2020 non-constant-luminance/PQ with centered
chroma. The working range and synthetic mastering tags are 10,000/0.005 nits;
they are not a measured display target or source master. No MaxCLL claim or
creative target mapping is supplied.

| Comparison | Observed result | Interpretation |
|---|---|---|
| Public-matrix/PQ scalar versus GPU reconstruction | Maximum 1 RGB48 code; PQ error about 1.056e-5 | Bounded arithmetic agreement |
| Decoded HDR10 RGB versus independent inverse NCL | Exact code agreement | Conversion check on this output |
| Reconstructed versus encoded/decoded RGB | Maximum 323 RGB48 codes | Encoding/conversion difference, not a movie-quality score |
| Initial stricter YUV error allowance | 4 codes observed against 3 allowed | Failed; retained as failure |

A previously registered compression allowance of four codes is separately
applied after observing this result, for diagnostic interpretation. It is not
new preregistered acceptance. Quantization, chroma conversion and encoding
remain visible rather than being called lossless or a measured quality gain.

## 2. Review findings and ownership checks

Pixel verification caught NUT labeling RGB48 bytes as RGB555 despite correct
output timing and HDR tags. The mux now uses the public pixel-format codec-tag
API. Review also found that self-consistent stage records could substitute a
wrong tool and empty input/output maps. The corrected checker requires exact
stage arguments, working directory, material loader/device environment, tool
identity, consumed inputs and emitted outputs. Corrected execution was rerun;
17 focused checker controls passed. Earlier evidence remains preserved.

The isolated Linux runner reuses unchanged Plurx process ownership source.
Cancellation and deadline controls on the wrapper's hold branch confirmed that
both helper and sleep descendant disappeared after awaited cleanup. They do
not test cancellation during active GPU or encoder work. The 30-second timeout
covers awaited execution after spawn; cleanup is awaited separately. Capture
length limits are not allocation-capacity or GPU-memory measurements. Container
two-CPU/two-GiB limits are experiment caps, not production admission evidence.

## 3. Retained evidence and replay scope

The bundle contains 207 ledger entries plus its ledger. Final receipt SHA256:
`2f9c83741c0074f5780dc3bc6788d6ae130892f6ed36c075ab817bce250d1642`.
Source pins, stage build commands, binary identities, actual output, failure
records and checker controls are retained. Frozen evidence can be checked
without rebuilding or rendering:

```sh
cd tools/dv_quality/backends/combined
PYTHONDONTWRITEBYTECODE=1 python3 check_combined.py evidence
PYTHONDONTWRITEBYTECODE=1 python3 test_combined.py evidence
```

Execution requires retained decoder/renderer prefixes and the recorded Docker
image. The FFmpeg 5.1.9/x265 3.5 encoder is an observed binary with 214 resolved
ELF dependencies, not a new source build. The standalone Rust 1.97.1 runner
retains its own resolved lockfile. This is not a clean dependency bootstrap;
source pins alone do not recreate the operating-system image. The slim evidence
checks recorded executable identities against its retained build identity;
it does not contain those executable ELF files.

## 4. Remaining implementation

This first combined control creates a GPU context per picture and admits only
the finite synthetic cohort. The next helper retains one context, accepts
actual bounded timestamped segments and allocates from validated raster sizes.
It must join real producer ownership, admission, generation and fallback before
playback can use it. Creative trims, MMR, general curves/matrices, metadata
reuse, unequal layer geometry, real-film quality and physical performance are
not established here. The production registry remains empty; no HDR10-E
badge or new playback route is authorized by this evidence.
