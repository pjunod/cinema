#!/bin/sh
# Compile qualification executables only. Does not run controls or touch devices.
set -eu
[ "$#" -eq 4 ] || { printf '%s\n' 'usage: build_target_controls.sh FFMPEG_PREFIX PLACEBO_PKGCONFIG_DIR DOVI_PREFIX OUTPUT_DIR' >&2; exit 2; }
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$4"
: "${CC:=cc}"
: "${CFLAGS:=-O2 -fno-math-errno}"
ffmpeg_flags=$(PKG_CONFIG_PATH="$1/lib/pkgconfig" pkg-config --static --cflags --libs libavformat libavcodec libavutil)
placebo_flags=$(PKG_CONFIG_PATH="$2" pkg-config --cflags --libs libplacebo)
"$CC" $CFLAGS -frounding-math -std=c11 -Wall -Wextra -Werror -I"$3/include" \
    "$source_dir/display_target_shader.c" -o "$4/display_target_shader" $placebo_flags -lm
"$CC" $CFLAGS -std=c11 -Wall -Wextra -Werror \
    "$source_dir/make_movie.c" -o "$4/make_movie" $ffmpeg_flags -lm -lpthread -ldl
