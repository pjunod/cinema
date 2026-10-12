#!/bin/sh
# Finite authored framework control; does not claim production packaging.
set -eu
out=${1:?Output directory required}
for bf in 0 2; do
  mkdir -p "$out/sdr-bf$bf"
  ffmpeg -hide_banner -loglevel error -nostdin -y \
    -f lavfi -i testsrc2=size=640x360:rate=24 \
    -f lavfi -i sine=frequency=440:sample_rate=48000 -t 24 \
    -c:v libx264 -threads 2 -preset medium -pix_fmt yuv420p \
    -b:v 1000k -maxrate 1500k -bufsize 2000k -g 48 -keyint_min 48 \
    -sc_threshold 0 -bf "$bf" -color_primaries bt709 -color_trc bt709 \
    -colorspace bt709 -color_range tv -c:a aac -b:a 96k \
    -f hls -hls_segment_type fmp4 -hls_time 2 -hls_playlist_type vod \
    "$out/sdr-bf$bf/index.m3u8"
done
# Actual outputs omit primaries/transfer. Retained as authored, not relabelled.
