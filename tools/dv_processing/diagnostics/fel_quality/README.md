# FEL quality diagnostics — offline measurements and retained source deltas

These tools accompany the [investigation ledger](../../../../docs/streaming/FEL-PICTURE-QUALITY-INVESTIGATION.md).
They do not change live playback or implement the separate packaging repair.
The evidence index records exact historical source hashes and private archive
locations. Media, decoded frames and binaries stay outside Git.

## What runs and what it establishes

| Tool | Purpose |
|---|---|
| `four_model_audit.py` | Hash-bound selected-frame R/F/C comparison in four explicit composition/colour models; not normative Dolby conformance. |
| `vector_metrics.py` | Shared signal-domain metric implementation used by the retained experiments. |
| `collect_rgb48.py` | Bounded raw-RGB transport, deterministic parity lattices and full frames 0/24. |
| `score_collections.py` | Hash/clock-bound 48-frame collection scoring, luminance strata and black-floor sensitivity. |
| `prepare_author_request.py` | Preserve actual encoded native packet clocks and ordered RPU hashes when preparing the frozen Rust-author request. |
| `audit_picture_payload.py` | Check all 48 coded-picture NAL sequences and clocks survive authoring; no presentation claim. |
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
