# Parsed RPU renderer bridge

This bounded probe feeds actual libdovi-parsed synthetic metadata to libplacebo API 374 and exports real software-Vulkan pixels. It does not qualify original-film FEL reconstruction, Dolby conformance, production frame association, or hardware performance. `qualified_fel` remains false.

## Replay

Use the approved renderer bundle's replay first. Then:

```sh
sh replay_parsed.sh /absolute/new-parser-scratch /absolute/renderer-replay
python3 test_check_parsed.py /absolute/new-parser-scratch
python3 test_image_identity.py
sh replay_cross.sh /absolute/new-cross-scratch /absolute/new-parser-scratch /absolute/authoring-evidence
sh replay_chroma.sh /absolute/new-chroma-scratch /absolute/new-cross-scratch
python3 test_chroma_control.py /absolute/new-chroma-scratch
python3 test_check_negatives.py
sh encode_hdr10.sh /absolute/new-cross-scratch
```

The authoring evidence must supply `decoded.yuv420p10le` and `adapted-rpu-frame0.nal` through `adapted-rpu-frame3.nal`. These are inputs produced by the separate source-only authoring probe. All replay writes go to newly-created scratch directories. The encoder only writes a previously-created cross replay scratch and refuses an existing encoded result.

Dependencies: the supplied ARM64 renderer replay image and prefix, checksum-verified dovi_tool source and C API header, resolved Cargo.lock. Each script consumes the prerequisite replay's `image-id.txt`, requires a lowercase SHA256 digest, and confirms that exact local Docker image is Linux ARM64. Parsed and cross replays propagate the identity; the encoder consumes the cross identity. Each successful stage writes its actual image and library identities in `*-build-identity.json`. A rebuilt prerequisite image can have a different valid identity. Rust 1.97.1 is explicitly checked; only source is compiled. The workspace append and resolved lock are the same changes as the CPU probe. Cargo registry checksums are locked. The existing image uses signed live Debian apt and version-pinned pip resolution, so rebuilding that image may drift until repository snapshots and package hashes are pinned. No host installation or runtime replacement occurs.

## Supported bridge subset

P7: guessed profile 7/FEL, fresh RPU with DM present, residual enabled, BL/EL 10-bit/VDR 12-bit, coefficient type 0/denominator 23, single identity polynomial with pivots 0/1023, LINEAR_DZ NLQ with fractional slope/threshold, and unconstrained `vdr_in_max=1`. Residual clipping controls are rejected because this pinned renderer's NLQ composition does not apply the RPU's `vdr_in_max` clipping. Missing/truncated metadata and requested missing EL are rejected before rendering. Each negative runs in a separate empty directory and requires exit status 1, the exact refusal diagnostic, no renderer events/setup logs, no frame writes and no unexpected artifacts. Crash/signal statuses, success, unrelated errors, renderer events and writes are explicitly tested as invalid evidence. Omitting EL as an explicit negative control demonstrates base-only output without acceptance.

P8: the separate probe admits fresh residual-disabled profile 8 metadata with the same identity polynomial/depths and no NLQ. Both probes use actual parsed nonlinear matrix/offsets and RGB-to-LMS matrix. This is not a complete RPU projector: MMR, nonidentity curves, reused RPUs, other depths/ranges, spatial filters and creative target trims are outside the experiment. L1 is read for distinct frame identity; L2/L8 counts are observable but no creative trims are applied. L5/L6/L9/L11/L254 are not transferred into rendering policy.

The native texture values are code/1023. Explicit `sample_depth=10,color_depth=10` makes the public color representation normalize DM offsets by 1024/1023. Omitting depth defaulted to8 and produced an approximately 0.00451 PQ mismatch; that was a harness error corrected before acceptance. The approved structured probe remains unchanged.

## Evidence and limits

Ten P7 positive/zero/omitted/shifted/disabled checks, each before mapping and through the full renderer, match the scalar arithmetic within 7.994e-6PQ and one RGB48 code. The scalar checker is an arithmetic control using parsed fixture fields and the documented shader transform, not an independent Dolby reference. Its bounds 2e-5PQ/2codes are diagnostic bounds chosen during this experiment after initial pixel observation; they do not establish media fidelity. Corruption regressions reject NaN/Inf inputs/outputs, malformed metadata, truncated floats/integers and wrong shifted integers.

Four distinct 64x64 decoded P8.1 pictures use four distinct RPUs tagged with L1 max/average values. Receipts bind each decoded-frame SHA, RPU SHA, parsed L1 and output SHA. Association is supplied by explicit frame index and does not prove encoded P7 BL/EL/RPU pairing or reordered decoder timestamps. Chroma uses point reconstruction at centered sample locations. Encoding now explicitly sets `chromal=center:filter=point`; relying on an unspecified output location was a harness error because [FFmpeg 5.1.9 zscale](https://raw.githubusercontent.com/FFmpeg/FFmpeg/n5.1.9/libavfilter/vf_zscale.c) defaults that case to left. Independent 16x16 native chroma-edge/ramp and scalar RGB-to-NCL controls prove actual center/left point sample phases separately from ffprobe tags: all samples match exactly, and left pixels mislabeled center are rejected. The RGB rounding bound of 1 code was selected before this control execution. The earlier encoded artifact is superseded; unchanged GPU RGB remains valid as arithmetic evidence. DV-on and standard HDR10 are rendered by the same GPU/backend: their observed 24-code maximum difference is difference-only evidence, with no acceptance bound. Fixed-point RPU matrices differ slightly from the standard HDR10 transform; this experiment alone does not isolate every contributor to that difference.

The HDR10 encoding endpoint uses FFmpeg 5.1.9/x265 3.5 Main10 QP0, four frames at 24 fps, BT2020 NCL/PQ limited420 centered chroma. RGB-to-YUV/chroma quantization remains present. Mastering primaries and 10000/.005 nits are synthetic harness target constants, not recovered source mastering information; MaxCLL/MaxFALL are not asserted. It encodes the parsed P8.1 GPU outputs, not an encoded P7 FEL reconstruction. The full renderer target is also10000/.005nits with clip mapping; no lower-peak HDR10 target or creative trims are qualified.

Next bridge: encoded P7 dual-layer access units, actual split/decode queues with rational PTS and per-frame RPU association, reordered frames and malformed/missing negatives, then the same renderer/encoder path. No qualification badge is warranted by the current receipts.
