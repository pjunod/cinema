# FEL quality diagnostics — offline measurements and retained source deltas

These tools accompany the [investigation ledger](../../../../docs/streaming/FEL-PICTURE-QUALITY-INVESTIGATION.md).
The no-encode RPU re-fit result is in [RPU-REFIT.md](RPU-REFIT.md).
They do not change live playback or implement the separate packaging repair.
The evidence index records exact historical source hashes and private archive
locations. Media, decoded frames and binaries stay outside Git.

## What runs and what it establishes

| Tool | Purpose |
|---|---|
| `capture_metadata.c` | Executed native-metadata observer source; 48 raw-RPU-bound frame records, with RGB lattice validation still pending. |
| `portable_comparison.py` | Produce the all-arm selected-frame result table with root aliases and measurement-derived admission reasons. |
| `capture_layers.c` | Local decoder-only source-layer capture with independent BL/EL software decoders, canonical planes and original clocks/RPUs. |
| `verify_layer_capture.py` | Rehash every captured plane/RPU and bind it to both retained events and case receipt; source identity only. |
| `four_model_audit.py` | Hash-bound selected-frame R/F/C comparison in four explicit composition/colour models; not normative Dolby conformance. |
| `vector_metrics.py` | Shared signal-domain metric implementation used by the retained experiments. |
| `collect_rgb48.py` | Bounded raw-RGB transport, deterministic parity lattices and full frames 0/24. |
| `score_collections.py` | Hash/clock-bound 48-frame collection scoring, luminance strata and black-floor sensitivity. |
| `prepare_author_request.py` | Preserve actual encoded native packet clocks and ordered RPU hashes when preparing the frozen Rust-author request. |
| `audit_picture_payload.py` | Check all 48 coded-picture NAL sequences and clocks survive authoring; no presentation claim. |
| `qualify_spatial_control.py` | Author, VCL/RPU/clock, native-frame and colour checks for a finished control, plus the AQ log tuple and SPS/PPS-derived effective QP-group receipts. |
| `lattice_eval.py` | All-48-frame stride-8 lattice scoring on the imported frozen operators; gates on exact 0/24 equality with retained full-resolution RGB before scoring. |
| `make_component_swaps.py` | Builds luma/chroma swap counterfactual YUV for the lattice evaluator; diagnostic only. |
| `portable_lattice.py` | Reduces a private lattice report to the committed portable JSON with neutral path prefixes. |
| `extract_served_refit.py` | Extracts the 48 Michael5400 RPUs from a captured Profile 8.1 publication, verifies the caller-supplied clock transform and binds copied BL slices to the source. |
| `served_refit_eval.py` | Scores the extracted served F+ curves and identity on the same frozen four-model lattice, bound to the M5 capture and helper comparison receipt. |
| `m7_pair_compare.py` | Compares the authored NP1 window with the retained served F+ result on the four frozen lattices and checks the prespecified margin and shadow limits. |
| `protocol-m7-michael5400-20261119.json` | Freezes the first Michael evaluation window, served F+ comparison, NP1 recipe, rate-only envelope and four-model margin and shadow limits. |
| `heldout_eval.py` | Frozen four-model scoring with observer metadata for fixtures that have no retained parsed metadata; full resolution or lattice; validated by exact reproduction of a retained result. |
| `land_rate.py` | Lands an arm in the 128.5–130.0 Mb/s authored band by changing only the ABR target (secant, at most 4 encodes); the iso-rate rule for every coding arm. `--target T` lands a diagnostic rung at T ± 0.75 Mb/s instead. |
| `make_export_oracle.py` | X0: oracle 10-bit 4:2:0 exports from the composition (rounded, truncated, swaps) and signed PRE−oracle plane statistics. |
| `roi_encode.c` | In-process libavcodec/libx265 encode carrying a per-frame quantOffsets map as ROI side data; options pass verbatim as one dictionary. Modes `lumazm` (zero-mean dark map) and the refinements `lumazx`/`lumazr`/`lumazf`/`lumazfn`/`lumazfs` (flat exclusion, ramp, FEL-benefit weighting from the BL planes). |
| `coding_loss_readout.py` | Portable per-arm readout of the iso-rate coding-loss arms and the protocols' accept rules, including the held-out stack rule. |
| `grain_ratio.py` | Report-only grain proxy: high-pass luma energy of the decode over the encoder input on 1–100 nit blocks. |
| `rpu_refit_eval.py` | RPU re-fit: fits single-layer 8.1 mapping curves (luma polynomial, chroma MMR; the safe monotone, continuous, ridge-regularised form is what ships) to the FEL composition on the untouched BL and scores them, plus the parsed-back curves of real RPUs, through the frozen four models. |
| `compare_composed.py` | Checks `segment_decode_render --dump-composed` (the production F+ composition and lattice) against the frozen `sample_codes`/`composition12` on the layer captures; exact equality or failure. |
| `segment_decode_render --export-composed-topleft` | Writes one 48-frame CPU Model B TopLeft 4:2:0 raw export and a clock-, RPU-, and frame-hash JSONL sidecar for the admitted Michael source domain. |
| `verify_topleft_export.py` | Binds the exact source, bundle, capture and sidecar, independently checks source color and every output code in relative frames 0 and 24, and verifies all 48 frame clocks and hashes. |
| `rpu_refit_write/` | Rust (dolby_vision 3.3.1): converts each source RPU to 8.1, writes the re-fitted curves (and, for decoder checks only, an optional L1 override), and prints them as parsed back from the written bytes. |
| `inject_rpu.py` | Puts one RPU NAL at the end of each access unit of an Annex B HEVC stream without B-frames (test clips only). |
| `dv_display_check.swift` | macOS: renders an AVPlayerLayer offscreen and reports mean linear RGB (optionally dumping the texture), to check whether the platform display path applies an 8.1 RPU's mapping. |
| `dv_patch_check.py`, `dv_patch_clips.sh` | Flat-patch decoder check: 48 patches played with the re-fit RPU, pre-mapped with the identity RPU, and unmapped; tells whether a decoder applies the curves as libplacebo does. |
| `encode_roi_control.py` | Translates a retained CLI recipe into the ROI harness, runs the byte-identity gates and the predeclared source-luma map arm. |
| `encode_spatial_control.py` | Fixed qg-size8 spatial-allocation control at the retained single-pass115/125 budget. |
| `encode_two_pass_control.py` | Two-pass allocation control with fresh 48-record statistics and identical requested coding settings across passes. |
| `encode_quantization_control.py` | Fixed RDOQ/psy-RDOQ control, separate output root; protocol committed before its results. |
| `encode_candidate.py` | Mac/Homebrew FFmpeg adapter for the recorded fixed controls; requires a separate output root. |
| `patches/` | Exact experimental reconstruction, export and observer deltas; not production patches or one cumulative patch stack. |

The model/scorer/collector and source deltas are exact retained bytes. The
encoder adapter changes output-root selection only; its original and adapted
hashes are recorded separately. Historical claims refer to the executed
producer in private evidence, not to an unexecuted adapted CLI.

```bash
python3 four_model_audit.py /absolute/path/to/bound-request.json  # NumPy required
python3 score_collections.py /absolute/request.json /absolute/result.json
python3 encode_candidate.py --help                             # Mac adapter options
```

Requests bind actual raw inputs, metadata, source RPUs, vector authorities,
clocks and hashes. The code deliberately refuses missing/mismatched inputs;
it does not download media or manufacture absent evidence. Original request
paths may need relocation using the private archive's location map, with a
new request/provenance record; do not rewrite sealed original receipts.

## Served F+ baseline and NP1 protocol

Keep the actual HTTP init and fragments, then concatenate their bytes into
the extractor's served MP4 input. Supply the capture events and the
source-clock offset established from the publication. The extractor's
receipt binds local bytes. Preserve the HTTP capture receipt with it.

Use the extractor's readback, raw RPUs and receipt with the capture planes,
metadata and exact helper comparison receipt in the scorer's request.
The scorer reports per-frame and pooled results for the active-region
lattice. Use those results to calibrate the per-title protocol.

Before scoring NP1, freeze separate calibration and evaluation windows,
the rate envelope, exact recipe and tool identities, and the measures for
pooled quality and shadow tails. Record how the served baseline and
measurement repeatability justify the improvement margin. The earlier
Z6 threshold belongs to the earlier comparison. The
[M7 plan](../../../../docs/streaming/FEL-RPU-REFIT-GRADUATION-AND-M7.md#milestone-7-sequence)
governs evaluation and full-title promotion.

## CPU TopLeft encode input

The `plurx-composed-model-b-topleft-v1` export applies the current FEL
composition at every luma site, then packs Model B's PQ RGB16 through
the BT.2020 NCL limited-range equations. The integer divisions round to
nearest with ties upward for Y, Cb and Cr. The RGB16 computation retains
Model B's signed floor shift. Chroma uses the even/even luma coordinates
without a smoothing filter. The output is full-raster little-endian
Y, Cb, Cr with ten significant bits per sample. The JSONL sidecar carries
PQ, BT.2020, NCL, video range, TopLeft and original source clock metadata.

This version requires the calibrated Michael source's stream declarations,
identity mapping, linear NLQ, exact YCC and LMS matrices and EOTF marker.
The codec declarations and source RPUs jointly bound the color
interpretation. Any incompatible source is refused. The author and encoder
must carry the output metadata explicitly. A raw export has no container
or bitstream color signaling.

Use a separate helper bundle and an empty output path. For the new
calibration window, use inclusive source ordinals 96 through 143.

```bash
"$HELPER" --export-composed-topleft "$SOURCE" 0 96 143 "$OUT/calibration.yuv" \
  >"$OUT/export.jsonl"
python3 verify_topleft_export.py "$OUT/request.json" "$OUT/verification.json" \
  --scratch-root "$TASK_TMP"
```

The verifier's request binds the source, the bundle and the unchanged
capture inputs by hash. It independently checks all 48 clocks, RPU
associations and raw hashes, and exact output codes on source ordinals
96 and 120. This establishes the new CPU representation. The earlier
native shader export remains separate historical evidence.

## Source deltas stay experimental

Patch paths name their original helper/libplacebo build contexts. They may
be alternate controls or depend on a preceding control; do not apply them
all blindly to the current tree. The ledger records unresolved absolute
composition units and filtering. Upstream libplacebo/helper attribution and
licenses remain applicable to their source deltas; no third-party source
archive or DoViBaker binary is bundled here.

The current source tree's Rust author remains the production implementation.
Its external diagnostic harness, pinned build receipt, original binary and
source archive are retained by hash in private evidence. No runtime binary
or media fixture is installed by these tools.

The extractor accepts `--window START END` as rational source seconds on the
1/1000 clock. Omission retains the original first window. A new-window
request must match its extraction receipt and all 48 captured frame clocks;
legacy receipts without an explicit window remain valid only for the original
window. Keep completed baseline requests and results unchanged.
