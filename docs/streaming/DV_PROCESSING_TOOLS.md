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

## Streaming interface

```text
segment_decode_render SOURCE OUTPUT_DIR VIDEO_INDEX MAX_FRAMES BL_W BL_H EL_W EL_H DEBUG OUTPUT_POLICY [NUT_OUTPUT]
```

`SOURCE` is a held descriptor for a finite, timestamped segment. `OUTPUT_DIR`
is a private generation directory. `VIDEO_INDEX` is the absolute stream index.
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
bounds. Arbitrary movies need the serving layer's interval, dependency preroll,
seek, cancellation and continuity handling. Passing an entire movie to this
finite helper is not that implementation.

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
