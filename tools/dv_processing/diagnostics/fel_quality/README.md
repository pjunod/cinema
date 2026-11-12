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
| `heldout_eval.py` | Frozen four-model scoring with observer metadata for fixtures that have no retained parsed metadata; full resolution or lattice; validated by exact reproduction of a retained result. |
| `land_rate.py` | Lands an arm in the 128.5–130.0 Mb/s authored band by changing only the ABR target (secant, at most 4 encodes); the iso-rate rule for every coding arm. `--target T` lands a diagnostic rung at T ± 0.75 Mb/s instead. |
| `make_export_oracle.py` | X0: oracle 10-bit 4:2:0 exports from the composition (rounded, truncated, swaps) and signed PRE−oracle plane statistics. |
| `roi_encode.c` | In-process libavcodec/libx265 encode carrying a per-frame quantOffsets map as ROI side data; options pass verbatim as one dictionary. Modes `lumazm` (zero-mean dark map) and the refinements `lumazx`/`lumazr`/`lumazf`/`lumazfn`/`lumazfs` (flat exclusion, ramp, FEL-benefit weighting from the BL planes). |
| `coding_loss_readout.py` | Portable per-arm readout of the iso-rate coding-loss arms and the protocols' accept rules, including the held-out stack rule. |
| `grain_ratio.py` | Report-only grain proxy: high-pass luma energy of the decode over the encoder input on 1–100 nit blocks. |
| `rpu_refit_eval.py` | RPU re-fit: fits single-layer 8.1 mapping curves (luma polynomial, chroma MMR; the safe monotone, continuous, ridge-regularised form is what ships) to the FEL composition on the untouched BL and scores them, plus the parsed-back curves of real RPUs, through the frozen four models. |
| `rpu_refit_write/` | Rust (dolby_vision 3.3.1): converts each source RPU to 8.1, writes the re-fitted curves, and prints them as parsed back from the written bytes. |
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
