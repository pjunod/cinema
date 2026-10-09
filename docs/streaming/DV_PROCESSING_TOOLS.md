# DV processing helpers — build and execution boundary

**Status:** built and reviewed helper mechanisms; serving integration, real-film
quality, sustained performance and physical DV acceptance remain open.

These tools reconstruct accepted P7 FEL pictures, stream timestamped RGB to an
encoder and attach adapted P8.1 metadata to that encoder's unchanged pictures.
They do not activate a playback route or establish that a worker can keep up.
The implementation and remaining work are tracked in
[DV_HDR_PROCESSING_STATUS.md](DV_HDR_PROCESSING_STATUS.md).

## Build the actual tools

Sources live in [`tools/dv_processing`](../../tools/dv_processing/). Build on
Linux with a C11 compiler, pkg-config and the following dependency installations:

| Dependency | Source used by the controls | Required configuration |
|---|---|---|
| FFmpeg | `bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa` | Static avcodec/avformat/avutil; HEVC decoder/parser; dovi_split, hevc_mp4toannexb and extract_extradata BSFs; Matroska/HEVC/MOV demuxers; Matroska/NUT/MP4 muxers; file protocol. No network, automatic dependency discovery, programs, avfilter or hardware decoder. |
| libplacebo | `0d043c7f6f79cd3687c023454bdacbe615e4d96f`, API 374 | Vulkan, glslang, dovi and xxhash enabled; libdovi integration, OpenGL, D3D11, demos and tests disabled. |
| libdovi | dovi_tool `83e1fdad6dcd5995556235946e7c5c0f9010d5a1` | Static `dolby_vision` C API with the corresponding reviewed `libdovi/rpu_parser.h`. |

The existing [renderer source fetcher](../../tools/dv_quality/backends/libplacebo/fetch_sources.py),
[FFmpeg source lock](../../tools/dv_quality/backends/decoded-layers/source-lock.json)
and [parser source lock](../../tools/dv_quality/backends/parsed-rpu/source-lock.json)
record archive identities. The parser lock also identifies the C header source.
Do not mix system FFmpeg headers with these static libraries. Reuse the existing
source-only dependency build described in the
[combined controls guide](DV_HDR_COMBINED_CONTROLS.md); enable the additional
NUT/MOV/MP4 components above before installing that FFmpeg prefix. Retained
prefixes were used for this change; a clean dependency bootstrap or production
container installation has not been qualified.

```sh
sh tools/dv_processing/build.sh \
  /absolute/ffmpeg-prefix \
  /absolute/placebo-prefix/lib/aarch64-linux-gnu/pkgconfig \
  /absolute/dovi-prefix \
  /absolute/output-bin
```

Use the installed pkg-config directory for the build architecture. The libdovi
prefix contains `include/libdovi/rpu_parser.h` and `lib/libdovi.a`. The script
builds with `-std=c11 -Wall -Wextra -Werror`. At runtime the dynamic loader must
find the exact libplacebo build and Vulkan driver; the controls used explicit
`LD_LIBRARY_PATH`, `VK_ICD_FILENAMES` and a private `XDG_RUNTIME_DIR`.

The helper build defaults to `CFLAGS=-O2`; callers may supply their own compiler
flags for diagnostics. Do not use fast-math flags: finite-value checks and exact
rounding are part of the processing contract.

## Streaming interface

```text
segment_decode_render SOURCE OUTPUT_DIR VIDEO_INDEX MAX_FRAMES BL_W BL_H EL_W EL_H DEBUG OUTPUT_POLICY [NUT_OUTPUT [START END MAX_PREROLL]]
```

`SOURCE` is a held descriptor for the original timestamped movie when `START`,
`END` and `MAX_PREROLL` are supplied; the older finite-input form remains available
for controls. `OUTPUT_DIR` is a private generation directory. `VIDEO_INDEX` is the absolute stream index.
The accepted output policy is `bt2020-pq-master-clip`: a BT.2020/PQ master-domain
representation, not a target display or a claim that creative trims were applied.

With `NUT_OUTPUT`, the renderer writes RGB48LE NUT packets directly to that held
pipe descriptor, flushing every picture. It keeps one GPU context and a bounded
BL/EL pairing window. The synchronous pipe supplies backpressure; no intermediate
RGB file is created. The consumer must run concurrently and the daemon must own
both children, the shared source fence and their summed resource admission.
Without `NUT_OUTPUT`, the diagnostic path writes `reconstructed.rgb48le` under
a 512 MiB scratch bound. `mux_rgb` exists for replaying that finite raw path.

Both modes write exact rational `timing.tsv` from decoder-observed PTS and
duration; those observations alone do not prove arbitrary stored-container
durations or served interval coverage. They also write fresh, accepted per-picture
`rpus/frame-NNN.nal` files. JSON observations remain on stdout and errors on
stderr; media is on the separate NUT descriptor. A nonzero exit, missing terminal
completion, missing frame or consumer failure invalidates the whole generation,
even when a partial media file or metadata files exist.

The current envelope is 1–64 pictures, BL at most 3840×2160, equal or half-size
EL, bounded HEVC packets and at most 16 queued pictures per layer. Streaming
removes the total RGB scratch bound; it does not remove frame/raster/source
bounds. Window mode seeks the original held source, decodes dependency preroll,
and emits only source PTS in `[START, END)`. Both endpoints are nonnegative `N/D`
with denominator at most `INT32_MAX`; the span is at most three seconds and
`MAX_PREROLL` is 0–512 paired pictures. Window mode also accepts `EL_W=EL_H=0`
to discover equal or half-size EL from the first actual decoded frame before GPU
initialization. The decoder pixel ceiling remains bounded by BL geometry. The three-second envelope admits the
normal 48-picture 24000/1001 entry, whose exact duration is 2.002 seconds.

Window reads share a 128 MiB aggregate budget across probing, seeks and decode;
a seek does not reset it. The file itself may be larger. IO staging is 64 KiB,
individual FFmpeg allocations are limited to 64 MiB, packets to 16 MiB, and
packet count to 4096. These bounds do not establish a total GPU/decoder-memory
budget. The daemon still owns deadlines, resource admission and cancellation.

The input access-unit trace is captured before both decoders. Each emitted
picture must match exactly one coded picture and its fresh RPU. The helper
exports original timestamps, coded and decoded hashes, and the exact RGB payload
hash. A later paired PTS at or beyond `END` proves the requested boundary.
Natural EOF requires the final observed picture to reach `END` and agree in both
directions with the declared video extent within one source tick. Unknown or
inconsistent extent refuses. These observations establish source membership;
the serving layer must separately verify its output frame grid and packaging.
NUT preserves source PTS here but does not serialize explicit packet durations;
`timing.tsv` remains the separate duration record.

The encoder must preserve the input time base (`-enc_time_base -1` in the
exercised FFmpeg path), source PTS and color interpretation. RGB is full-range
BT.2020/PQ; the encoder control explicitly converts to limited BT.2020 NCL,
center-sited YUV420P10 before Main10 encoding. Applying an additional DV reshape
to these reconstructed pixels would apply that operation twice.

For P8.1, run `author_p81` on the encoded video with this same generation's
timing and RPUs. Its [authoring contract](../../tools/dv_processing/AUTHORING.md)
describes the exact association, unchanged coded base, metadata adaptation,
explicit Matroska presentation durations and final MP4/HLS verification still
required. Its output is an intermediate, not a served session.

## Focused validation and limits

```sh
cc -std=c11 -Wall -Wextra -Werror \
  tools/dv_processing/test_nut_timing.c -o /tmp/test-nut-timing
/tmp/test-nut-timing
python3 tools/dv_processing/test_stream.py \
  --helper /absolute/output-bin/segment_decode_render \
  --source tools/dv_quality/backends/combined/source.mkv \
  --output /absolute/new-control-directory
```

The streaming command requires FFmpeg/ffprobe with libx265 and zscale on PATH
and the runtime loader/driver environment above. It compares actual raw and
pipe-rendered bytes, checks encoded exact PTS/RPU association and checks the
actual SIGPIPE result after the consumer closes. `--upper-source` optionally
adds the separately generated 3840×2160 BL/1920×1080 EL one-picture control;
a general 4K movie is not a substitute for that synthetic input.

Validation retained for this change:

- The frozen renderer passed 28 focused guard/mux controls. A nearest-neighbor
  tie bug originally produced a 1197-code error; a deterministic positive
  1/64 reference-pixel bias corrected it. All 80 sampled 4K points, including
  40 tie points, then agreed within one RGB code at the unchanged bound.
- A separate spatial chroma control compared all 49,152 pixels within one code.
  This defines the chosen point-sampling policy, not normative Dolby interpolation.
- Actual streaming controls passed raw/pipe byte equality, reordered encode
  with exact source PTS, closed-consumer refusal and one real 4K encoded picture.
- Review added checked NUT clock scaling, including the complete interval end;
  focused integer-boundary tests and the final small pipe controls passed.
- P8.1 authoring passed source-bound reordered six-picture and one-picture 4K
  controls, including unchanged encoded base bytes and decoded HDR10 pixels.

The controls used software Vulkan and a two-CPU/two-GiB container limit.
Those are experiment caps, not measured consumption or production concurrency
budgets. No incremental FEL-on/off benchmark, real-time movie result, physical
DV rendering or independent Dolby conformance result follows from these checks.
The production default Jellyfin FFmpeg 8 runtime still needs end-to-end acceptance;
the exercised encoder was the retained FFmpeg 5.1.9/libx265 3.5 control runtime.

Only focused controls run on this task. The full suite runs once on the completed,
frozen effort branch; Windows validation is waived for this effort. No merge
coordinator handoff is authorized.

## Master-domain metadata policy

The accepted reconstruction subset is fresh P7 FEL with the documented identity
reshape and source matrix, 10-bit BL/EL and 12-bit reconstructed signal. The
renderer applies the FEL residual and source color conversion. It does not apply
Dolby display trims, crop the picture, or certify a Dolby display mapping.

L2 trims, L3 statistics offsets, opaque L4 anchors, a valid nonempty L5 active
rectangle and short L8 trims (length 10) may accompany this master reconstruction.
Their original values remain in the exported RPU. The P8.1 author verifies exact
preservation of these fields while removing the mapping already baked into the
new base pictures. HDR10 output cannot carry those Dolby display instructions;
its effective report must keep creative-trim application false. Longer L8 forms
and the other unsupported metadata forms still refuse.

The pinned renderer omits `vdr_in_max` clipping. Admission therefore proves that
clipping cannot bind for **any** supported 10-bit EL code, using the maximum
LINEAR_DZ residual at the farthest endpoint. The decision uses integer arithmetic
and reserves 2^-16 rounding headroom for the binary32 shader. A limit of 0.125
with offset 512, slope 2048/2^23 and zero threshold passes; a limit that actually
clips the same residual refuses. The focused arithmetic regression is:

```sh
python3 tools/dv_processing/test_metadata.py
```

This is a supported-subset policy, not a claim that every P7 movie, or every
Dolby Vision profile, can use the helper. Unsupported input retains the ordinary
playback fallback.

The focused retained-metadata graph replay uses generated 64×64 BL/EL controls,
not copied movie content. It covers a nonzero-start original-source window,
pre-GPU metadata refusal, inconsistent EOF extent, actual P8.1 RPU roundtrip,
unchanged encoded base and unchanged DV-disabled decoded pixels:

```sh
sh tools/dv_processing/controls/replay.sh \
  /absolute/new-scratch /absolute/accepted-dependencies /absolute/pinned-dovi-source
```

The dependency root contains `ffmpeg-prefix`, `prefix`, `include` and `lib` as
above. This focused ARM64 control recipe uses the retained image
`sha256:96951921bc396f9fb96e579d9c15b83abe8b488390f8a7851c3754b2d96114c9`,
Rust 1.97.1 and the committed public fixture-generator lockfile. Its separate
software encoder is FFmpeg 5.1.9/x265 3.5. The recipe refuses an existing scratch
directory and copies only control inputs and helper sources into the new one.
It is not a clean deployment bootstrap or a substitute for native worker checks.

## One-fragment P8.1 packaging

The author accepts an optional final `fmp4` argument. It writes one init and one
fragment, retaining absolute timestamps and the adapted per-frame RPU. This
mode requires a keyframe-led, non-reordered, uniform contiguous frame grid.
After the header establishes the output clock, every PTS and duration must
rescale exactly, spacing must be positive and smaller than `INT_MAX`, and the
complete endpoint must fit `INT64_MAX`. An init-only file left by refusal must
never be published. The serving layer owns explicit source-to-output mapping.

The sample entry is `hev1`, because encoded base packets retain in-band parameter
sets. Retagging these samples as `hvc1` would misdescribe them. Device acceptance
and final HLS publication remain integration checks; this helper alone does not
prove them. The focused replay includes the actual author's output-clock guard
with endpoint, nonunit input clock and timestamp-spacing boundaries.
