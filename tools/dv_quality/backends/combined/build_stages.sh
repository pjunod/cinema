#!/bin/sh
set -eu
cd /work
PKG_CONFIG_PATH= PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig cc -std=c11 -Wall -Wextra -Werror mux_rgb.c -o mux_rgb $(PKG_CONFIG_PATH= PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig pkg-config --cflags --libs libavformat libavcodec libavutil)
cc -std=c11 -Wall -Wextra -Werror decode_layers.c -o decode_layers $(PKG_CONFIG_PATH=/work/ffmpeg-prefix/lib/pkgconfig pkg-config --static --cflags --libs libavformat libavcodec libavutil)
cc -std=c11 -Wall -Wextra -Werror -I/work/include fel_export.c /work/lib/libdovi.a -o fel_export $(PKG_CONFIG_PATH=/work/prefix/lib/aarch64-linux-gnu/pkgconfig pkg-config --cflags --libs libplacebo) -lm -lpthread -ldl
