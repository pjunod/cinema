# Dolby Vision nonidentity authoring — bounded M0 reshape controls

**Status:** open — bounded affine mechanics reviewed; product qualification pending
· **Updated:** 2026-10-08

Companion to the [identity-authoring controls](DV_HDR_AUTHORING_CONTROLS.md)
and [implementation ledger](DV_HDR_PROCESSING_STATUS.md). This experiment tests whether destination metadata describes an
already reconstructed base without applying the original reshape a second
time. Its reference is a deliberately limited mathematical fixture, not a
commercial Dolby Vision grade or a profile-conformance oracle.

## 1. The controlled operation

The source luma curve is `g(x) = 1/16 + 3x/4`; chroma mapping is identity and
the enhancement samples contribute a known positive or negative residual.
For the selected integer samples, the once-reconstructed 12-bit luma is
`256 + 3 × BL_Y + 4 × residual_code`. A separate scalar implementation checks
the CPU reconstruction rather than treating changed pixels as proof of FEL.

The corrected P8.1 control disables the residual and uses identity mapping
for the reconstructed, encoded base. The deliberately wrong control also
disables the residual but retains the original affine luma curve. That curve
then operates on an already reconstructed base, making repeated reshaping
observable. Four distinct pictures and metadata tags bind every comparison.

This is an affine control. Passing it does not establish metadata adaptation
for arbitrary nonlinear curves, nonstandard matrices or creative trims. The
bounded helper refuses unsupported matrices and trim metadata explicitly.
The CPU implementation does not apply all `rgb_to_lms` behavior, so the fixed
standard-matrix restriction remains mandatory. The GPU receives explicitly
paired decoded YUV/RPU inputs and fixture frame indices; this authoring test
does not itself prove container DV configuration or original P7 timing.

## 2. Read each comparison at its own stage

| Comparison | Expected interpretation |
|---|---|
| CPU reconstruction versus independent integer equations | Correct application of the declared affine map and residual |
| Source RGB to YUV versus scalar BT.2020 NCL | Correct conversion into the compatible encoded base |
| Decoded native YUV versus encoder input | Compression error, separate from color conversion |
| Decoded HDR10 RGB versus scalar inverse NCL | Correct DV-disabled base interpretation |
| DV-on GPU output versus separate matrix/curve equations | Agreement for the supported synthetic metadata subset |
| Deliberately wrong versus once-mapped output | A negative control for applying the source reshape again; not a movie-quality score |

The final replay reports exact CPU agreement with its integer reference.
Forward NCL conversion differs by zero native YUV codes, encoding adds at
most two YUV codes, and decoded HDR10 RGB differs from scalar inverse NCL by
at most one RGB48 code. Source-to-decoded RGB differs by up to 245 codes,
within the declared 1,065-code arithmetic allowance. These comparisons measure
different stages and must not be substituted for one another.

The full-matrix GPU control differs from its scalar reference by at most
8.8093e-6 PQ and one RGB48 code. The deliberately repeated affine map differs
from the once-mapped output by 2,943–2,962 RGB48 codes, exceeding the declared
1,024-code minimum separation. The correct and deliberately wrong paths each
match their own arithmetic equations; only the correct path matches the
once-mapped reference. This demonstrates a detectable repeated operation,
not a measured quality improvement over a movie's base layer.

Arithmetic bounds are immutable and finite. Their original selection
chronology is declared from the session's command order; it is not independently
timestamped preregistration. Each new replay hashes the fixed bounds before
processing so later alteration is detectable. The RGB allowance follows the
stated conversion/compression-error bound; it is not a perceptual threshold.

## 3. Reproduce and validate

The replay requires approved CPU and parsed-GPU prerequisites. CPU fixture
creation/reconstruction uses Linux AMD64; rendering and encoding use Linux
ARM64. The dependency contract validates exact source/library inventories,
relative linker symlinks, image identity and platform, then validates copied
artifacts again. Unlisted source/configuration files cannot silently alter the
build. Changed dependencies require their own reviewed identity evidence.

```bash
export PYTHONDONTWRITEBYTECODE=1
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
bash tools/dv_quality/backends/nonidentity-authoring/replay.sh \
  "$scratch/nonidentity" "$cpu_replay" "$parsed_replay"
```

Both prerequisite variables must be absolute paths to the corresponding
approved replay roots. The new scratch directory retains newly generated
fixtures, encoded and decoded pictures, actual GPU outputs, checker results
and dependency identities. Existing destinations are refused. The replay
runs the scientific and contract corruption controls as part of acceptance.

Actual consumed per-frame bytes, raw RPU, parsed curve, typed parser/render
events and output hashes must agree. All RGBA channels and every numerical
limit must be finite. A missing frame, wrong/correct output substitution,
modified metadata, wrong curve or numeric replacement for a boolean flag must
fail. Arbitrary replacement with a scalar oracle before receipt generation is
not a separately tested scientific negative; the retained artifact ledger
checks byte identity, not the claimed origin of newly generated pixels. Unsupported
metadata passes its negative test only with the exact clean refusal and no
successful renderer events or output artifacts. A crash is not acceptance.

The HDR10 check inspects observed transfer, primaries, matrix, range, chroma
location, depth, profile, frame count and dimensions in addition to its pixel
comparisons. A valid DV-enabled render cannot compensate for a broken base.

## 4. Final source and review provenance

The [retained receipt](../../tools/dv_quality/backends/nonidentity-authoring/receipt.json)
and its 212-entry ledger accompany 213 source/evidence files. The full fresh
replay executed the scientific source recorded in its 28-entry manifest and
passed 16 scientific checks, 14 replay-contract checks and six injector checks.
The final package has a separate 28-entry source manifest. Its exact three-file
delta changes provenance wording and handling/tests for absent empty output
directories; it does not change the executed CPU/GPU stages.

The final 16 replay-contract checks cover that packaging change and were
independently rerun by the reviewer. Both manifests, every retained artifact
hash and the explicit post-replay delta were verified. This is not a claim
that the earlier full replay executed the final wrapper bytes unchanged.

Review also corrected non-finite alpha/limits, incomplete per-frame input and
curve binding, insufficient prerequisite/symlink checks, permissive negative
exit handling and incomplete HDR10 signaling checks. Targeted corruption
controls cover those findings. Original dependency bundles remain unchanged.

## 5. Remaining acceptance

General nonlinear authoring, supported artistic trims, independent Dolby
picture references and container/profile conformance remain unqualified.
Matched-rate movie-quality measurements, target-display mapping and physical
performance also remain open. This evidence does not enable production
routing, award HDR10-E or replace compatible HDR10/base-copy fallbacks.
