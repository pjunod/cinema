# Bounded reconstructed P7 → P8.1 authoring

**Status:** built offline helper; serving admission and physical Dolby Vision
acceptance remain open. Source reconstruction admission belongs to the renderer.

`author_p81.c` accepts a video-only encoded HEVC container, the successful
renderer generation's rational `timing.tsv`, and its `rpus/frame-NNN.nal` files.
It writes a private Matroska intermediate with P8.1 configuration. The caller
must keep source, metadata, and output under one generation fence and publish
only after exit zero and output verification. It must preserve existing native
DV, HDR10, and compatible-base fallbacks on every refusal.

```sh
author_p81 encoded.mp4 timing.tsv rpus authored.mkv
P81_HELPER=./author_p81 P81_ENCODED=encoded.mp4 \
  P81_TIMING=timing.tsv P81_RPUS=rpus python3 test_author.py
```

The helper associates encoded packets with renderer frames by exact rational
PTS, retaining coded order and encoder DTS. It adapts each corresponding fresh
P7 FEL RPU with libdovi mode 2 (`To81`) and removes the mapping already applied
during reconstruction. Each receipt row records source and adapted RPU SHA-256.
It appends the adapted UNSPEC62 NAL after the original encoded access unit and
writes profile 8, residual-independent, BL-present/EL-absent, HDR10 compatibility
ID 1 configuration. The base packet bytes and HDR10 SEI remain unchanged.

MP4 encoder sample durations are not authority for presentation intervals.
In the retained reordered control the encoder preserved the source's rounded
PTS (0, 42, 83, 125, 167, 208 ms), but exposed a nominal 2657/64000 second
packet duration instead of the renderer's explicit 41 ms. The intermediate
therefore uses Matroska BlockDuration for the renderer's independent interval,
while retaining encoder packet order and DTS at mux input. It refuses if either
encoder or output clock cannot exactly represent the source PTS/duration.
Any final MP4/HLS packaging needs its own interval/configuration verification;
this helper is not evidence that arbitrary source gaps fit MP4 sample tables.

The bound is 64 frames, 3840×2160, one complete picture per length-prefixed HEVC
packet, 32 MiB per packet and 64 KiB per RPU. Inputs must be BT.2020/PQ limited
range HEVC. Existing RPUs, EL NALs, repeated/missing PTS, missing frames, partial
NALs, unsupported layers/temporal IDs, missing source metadata and creative
L2/L8 or complex L3/L4/L10/L255 metadata refuse. DV level is selected from the
shortest actual interval and raster using FFmpeg's levels 1–9 table; each
access unit must fit that level's high-tier peak bitrate bound. The renderer
retains the stronger mapping, signal, active-area and source-pair guards.

## Evidence and limits

The final helper compiled with `-std=c11 -Wall -Wextra -Werror` against the
retained FFmpeg 9 static prefix and libdovi C API. It does not mix FFmpeg 5
system libraries into the reconstruction process. The runtime control image is
SHA-256 `96951921bc396f9fb96e579d9c15b83abe8b488390f8a7851c3754b2d96114c9`;
its software encoder is FFmpeg 5.1.9/libx265 3.5. Production's default Jellyfin
FFmpeg 8 encoder remains a separate deployment qualification requirement.

Focused regression passed for six reordered source-bound reconstructed 64×64
pictures and one actual 3840×2160 reconstructed picture. It checks P8.1
configuration, exact presentation intervals, RPU source/adapted hashes, distinct
per-frame dynamic metadata, unchanged encoded base payload and bit-identical
HDR10-only decoded YUV. Duplicate timing, absent RPU and already-DV encoded
input refuse. The one-picture 4K run establishes raster/resource functionality;
the six-picture control establishes reorder association. Synthetic decoded
source controls do not establish movie quality, sustained throughput, physical
DV-client rendering, independent reference conformance, or serving readiness.
