#!/bin/sh
# Compile the helpers against explicitly selected dependency installations.
set -eu
[ "$#" -eq 4 ] || { echo 'usage: build.sh FFMPEG_PREFIX PLACEBO_PKGCONFIG_DIR DOVI_PREFIX OUTPUT_DIR' >&2; exit 2; }
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ffmpeg_prefix=$1
placebo_pc=$2
dovi_prefix=$3
output_dir=$4
mkdir -p "$output_dir"
: "${CC:=cc}"
: "${CFLAGS:=-O2}"
# pkg-config deliberately expands these compiler/linker argument lists. The
# dependency installations and CFLAGS are trusted build inputs, never playback requests.
ffmpeg_flags=$(PKG_CONFIG_PATH="$ffmpeg_prefix/lib/pkgconfig" pkg-config --static --cflags --libs libavformat libavcodec libavutil)
placebo_flags=$(PKG_CONFIG_PATH="$placebo_pc" pkg-config --cflags --libs libplacebo)
"$CC" $CFLAGS -std=c11 -Wall -Wextra -Werror -I"$dovi_prefix/include" \
    "$source_dir/segment_decode_render.c" "$dovi_prefix/lib/libdovi.a" \
    -o "$output_dir/segment_decode_render" $ffmpeg_flags $placebo_flags -lm -lpthread -ldl
"$CC" $CFLAGS -std=c11 -Wall -Wextra -Werror "$source_dir/mux_rgb.c" \
    -o "$output_dir/mux_rgb" $ffmpeg_flags -lm -lpthread -ldl
if [ -f "$source_dir/author_p81.c" ]; then
    "$CC" $CFLAGS -std=c11 -Wall -Wextra -Werror -I"$dovi_prefix/include" \
        "$source_dir/author_p81.c" "$dovi_prefix/lib/libdovi.a" \
        -o "$output_dir/author_p81" $ffmpeg_flags -lm -lpthread -ldl
fi
