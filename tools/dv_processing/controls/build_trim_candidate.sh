#!/bin/sh
# Compile-only entry point. Neither executable is run or shipped by this script.
set -eu
[ "$#" -eq 4 ] || { echo 'usage: build_trim_candidate.sh FFMPEG_PREFIX PLACEBO_PC DOVI_PREFIX OUTPUT_DIR' >&2; exit 2; }
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ffmpeg_prefix=$1
placebo_pc=$2
dovi_prefix=$3
output_dir=$4
mkdir -p "$output_dir"
: "${CC:=cc}"
: "${CFLAGS:=-O2 -fno-math-errno}"
ffmpeg_flags=$(PKG_CONFIG_PATH="$ffmpeg_prefix/lib/pkgconfig" pkg-config --static --cflags --libs libavutil)
placebo_flags=$(PKG_CONFIG_PATH="$placebo_pc" pkg-config --cflags --libs libplacebo)
for instrument in trim_candidate test_trim_candidate; do
    "$CC" $CFLAGS -frounding-math -std=c11 -Wall -Wextra -Werror -I"$dovi_prefix/include" \
        "$source_dir/$instrument.c" "$dovi_prefix/lib/libdovi.a" \
        -o "$output_dir/$instrument" $ffmpeg_flags $placebo_flags -lm -lpthread -ldl
done
