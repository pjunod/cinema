# Dolby Vision parsed metadata and authoring — bounded M0 controls

**Status:** open — synthetic mechanics implemented; decoded P7 and product
qualification remain incomplete · **Updated:** 2026-10-08

This follows the [first backend controls](DV_HDR_BACKEND_CONTROLS.md).
It records actual metadata parsing, reconstructed-base encoding and a separate
DV-on render of the encoded picture. The [implementation ledger](DV_HDR_PROCESSING_STATUS.md)
tracks the remaining project; the [build contract](DV_HDR_PROCESSING_BUILD.md)
still governs admission to a product route.

## 1. What these experiments establish

| Operation | Retained evidence | Boundary |
|---|---|---|
| Parsed synthetic P7 RPU to libplacebo | Real libdovi parser, guarded field projection, BL/EL textures and ten arithmetic output comparisons | Identity reshaping, bounded NLQ and synthetic textures; no encoded P7 decode or timestamp association |
| Reconstructed picture to authored base | Four distinct pictures encoded as Main10, four distinct generated/adapted RPUs inserted and parsed, DV-disabled decode compared to scalar BT.2020 equations | Raw elementary stream with no B-frame reorder; no container DV configuration or general profile-conformance proof |
| Encoded picture rendered with DV enabled | Four decoded frames and their parsed RPUs rendered by libplacebo, with actual input/output hashes | Explicit frame indices; same-renderer DV-on/off differences do not establish fidelity to a Dolby reference |
| GPU picture to HDR10 elementary output | Main10 encode, PQ/BT.2020 limited-range tags and explicit centered chroma sampling | Synthetic static mastering constants, no measured content-light analysis or lower-peak creative mapping |

The original P7 fixture has **identity reshaping**. The experiment therefore
cannot detect double application of arbitrary nonlinear reshaping. Adapted
metadata disables the residual and retains the fixture's identity mapping;
that is narrower than authoring valid metadata for an arbitrary reconstructed
movie. Nonidentity reshaping is the next authoring control, not a completed
acceptance item.

## 2. Read the numerical evidence

The parsed-P7 arithmetic controls differ from their scalar equation reference
by at most one RGB48 code. The authoring experiment's standard BT.2020 NCL
conversion and inverse conversion match their scalar controls exactly; the
encoded YUV differs by at most two codes from its input. RGB after encoding
and decoding differs from the reconstructed source by up to 219 of 65,535
codes. These are distinct comparisons at different pipeline stages.

Across the four frames, DV-on versus standard HDR10 through the same GPU
renderer differs by up to 24 RGB48 codes. Its HDR10 output differs from the
independent scalar inverse-NCL calculation by at most one code. Fixed-point
RPU matrices and the standard HDR10 transform are not identical. No independent
Dolby movie render is available to turn the 24-code difference into a fidelity
score or a quality improvement.

The PQ/RGB replay bounds and authoring diagnostic caps were chosen during the
experiment after observing output. They detect drift in these controls; they
are not preregistered quality thresholds. The receipts leave picture-reference,
profile-conformance and performance qualification null or false.

## 3. Reproduce in scratch

Use Linux ARM64 Docker support and Python with safe tar data extraction.
The first renderer replay builds its own image and library prefix. The parsed
bridge consumes that replay's validated image identity; it must not depend on
a pre-existing developer image. Public sources are pinned and hash checked;
live Debian/PyPI resolution is still disclosed rather than called a locked OS
snapshot.

```bash
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
sh tools/dv_quality/backends/libplacebo/replay.sh "$scratch/renderer"
cp -R tools/dv_quality/backends/p81-authoring "$scratch/authoring"
bash "$scratch/authoring/replay.sh"
sh tools/dv_quality/backends/parsed-rpu/replay_parsed.sh \
  "$scratch/parsed" "$scratch/renderer"
sh tools/dv_quality/backends/parsed-rpu/replay_cross.sh \
  "$scratch/cross" "$scratch/parsed" "$scratch/authoring"
sh tools/dv_quality/backends/parsed-rpu/replay_chroma.sh \
  "$scratch/chroma" "$scratch/renderer"
sh tools/dv_quality/backends/parsed-rpu/encode_hdr10.sh "$scratch/cross"
```

The authoring replay writes into its copied directory. Its first cross-render
check validates **retained historical GPU snapshots**, bound to the input and
output hashes; it does not invoke the GPU. The subsequent parsed/cross replay
above generates new GPU outputs. Each stage records the actual image and
library identities used. Never run the authoring replay inside its tracked
source bundle.

Validate the new GPU outputs separately from the retained snapshot:

```bash
mkdir "$scratch/fresh-validation"
for name in check_cross_render.py source-definition.json \
  analytic-hdr10-reference.rgb48le decoded.yuv420p10le; do
  cp "$scratch/authoring/$name" "$scratch/fresh-validation/$name"
done
python3 "$scratch/authoring/import_cross_render.py" \
  --source "$scratch/cross" \
  --destination "$scratch/fresh-validation/cross-render"
python3 "$scratch/fresh-validation/check_cross_render.py"
```

The importer requires an explicit new destination and validates its bounded
33-file input inventory before copying. Importing is not validation; the last
command binds and checks the new pixels against the authoring inputs.

See the parsed bridge's [README](../../tools/dv_quality/backends/parsed-rpu/README.md)
for its exact metadata subset. The authoring [receipt](../../tools/dv_quality/backends/p81-authoring/receipt.json)
records the reference equations, failed encoder attempts, metadata constants
and each unmet operation. The two branches use different synthetic mastering
constants; neither claims to recover or analyze a movie's mastering metadata.

## 4. Evidence integrity and review

The authoring checker requires exactly one reconstruction row and one rendered
row for each of four distinct frames. Each row binds actual output hashes,
decoded input hashes, RPU identity and parsed L1 tags. Replacing GPU pixels with
a scalar reference, swapping frames, changing a pixel, duplicating a stage or
omitting a stage fails before any diagnostic comparison can pass.

The four RPUs use distinct generated L1 tags solely to prove metadata identity.
They are not artistic metadata. Raw HEVC has no container PTS; the declared
24-fps timeline and no-reorder access-unit association do not prove decoder
reordering, seeking, variable frame rate or discontinuity handling.

Review found and corrected output hashes that were recorded but not checked.
It also found a replay dependency on a locally available image, chroma sample
siting that disagreed with the encoded tag, and negative controls that accepted
any nonzero exit instead of the intended clean refusal. These mechanisms have
separate focused controls; a process crash cannot count as supported rejection.

Independent review approved both final bundles for the bounded synthetic
operations above. The parsed bridge includes five pixel-corruption, six image-
identity, six refusal and three chroma-control regressions. The authoring bundle
includes nine output-binding and ten portable-import controls in addition to
its picture and metadata association checks. Fresh isolated replays passed.

## 5. Work still required

- Encode and decode real synthetic P7 BL/EL access units with actual rational
  timestamps, RPU association, reordering and missing/malformed controls.
- Exercise nonidentity reshaping through reconstruction and destination
  metadata adaptation, independently checking both DV-on and DV-off pictures.
- Establish target mapping, supported creative metadata, independent references
  and container/profile validity for each admitted operation.
- Measure matched-rate quality and full-graph resource costs on representative
  content and hardware before implementing production admission and badges.

The existing compatible HDR10 and base-copy P8.1 fallbacks remain the product
behavior. These tools do not grant the HDR10-E badge.
