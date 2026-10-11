#!/bin/sh
set -eu
cd /work
export PKG_CONFIG_PATH=/work/ffmpeg-prefix/lib/pkgconfig
for helper in mux_timeline decode_timeline; do
  cc -std=c11 -Wall -Wextra -Werror /work/$helper.c -o /work/$helper $(pkg-config --static --cflags --libs libavformat libavcodec libavutil)
done
for b in 0 2; do
  ./mux_timeline bl-vfr-b$b.mkv el-vfr-b$b.mkv rpu-tags compound-vfr-b$b.mkv 0 6 normal > mux-vfr-b$b.jsonl 2> mux-vfr-b$b.stderr
done
for s in 0 1; do
  ./mux_timeline bl-segment$s.mkv el-segment$s.mkv rpu-tags compound-segment$s.mkv $((3*s)) 3 normal > mux-segment$s.jsonl 2> mux-segment$s.stderr
done
./mux_timeline bl-segment1.mkv el-segment0.mkv rpu-tags compound-cross-epoch-el.mkv 3 3 normal > mux-cross-epoch-el.jsonl 2> mux-cross-epoch-el.stderr
./mux_timeline bl-vfr-b2.mkv el-vfr-b2.mkv rpu-tags compound-implicit.mkv 0 6 implicit-duration > mux-implicit.jsonl 2> mux-implicit.stderr
./mux_timeline bl-vfr-b2.mkv el-vfr-b2.mkv rpu-tags compound-swapped-rpu.mkv 0 6 swapped-rpu > mux-swapped-rpu.jsonl 2> mux-swapped-rpu.stderr
sh /work/decode_cases.sh
