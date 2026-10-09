#!/bin/sh
# Install renderer-only libraries, provenance, licenses and source for relinking.
set -eu
[ "$#" -eq 2 ] || { echo 'usage: build-bundle.sh DEPENDENCY_PREFIX OUTPUT_ROOT' >&2; exit 2; }
deps=$1
out=$2
case "$deps:$out" in /*:/*) ;; *) echo 'bundle paths must be absolute' >&2; exit 2 ;; esac
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$out/bin" "$out/lib" "$out/licenses/plurx" "$out/sources"
notices=${PLURX_DV_PROJECT_NOTICES:-"$source_dir/../.."}
if [ ! -f "$notices/LICENSE" ]; then notices="$source_dir/../licenses/plurx"; fi
cp "$notices/LICENSE" "$notices/NOTICE" "$out/licenses/plurx/"
placebo_pc=$(find "$deps/placebo/lib" -name libplacebo.pc -print)
[ "$(printf '%s\n' "$placebo_pc" | wc -l)" -eq 1 ] && [ -f "$placebo_pc" ]
sh "$source_dir/build.sh" "$deps/ffmpeg" "$(dirname "$placebo_pc")" "$deps" "$out/bin"
placebo_lib=$(PKG_CONFIG_PATH="$(dirname "$placebo_pc")" pkg-config --variable=libdir libplacebo)
cp -L "$placebo_lib/libplacebo.so.374" "$out/lib/libplacebo.so.374"
cp "$deps/lib/libdovi.a" "$out/sources/libdovi.a"
# Static FFmpeg decode libraries must never resolve against Jellyfin/system headers.
ffmpeg_flags=$(PKG_CONFIG_PATH="$deps/ffmpeg/lib/pkgconfig" pkg-config --static --cflags --libs libavformat libavcodec libavutil)
placebo_flags=$(PKG_CONFIG_PATH="$(dirname "$placebo_pc")" pkg-config --cflags --libs libplacebo)
cat > "$out/sources/abi.c" <<'C'
#include <stdio.h>
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avutil.h>
#include <libplacebo/config.h>
int main(void) {
    printf("{\"libplacebo\":%u,\"avcodec\":%u,\"avformat\":%u,\"avutil\":%u}\n", PL_API_VER,
        avcodec_version() >> 16, avformat_version() >> 16, avutil_version() >> 16);
    return 0;
}
C
cc -std=c11 -Wall -Wextra -Werror "$out/sources/abi.c" -o "$out/sources/abi" $ffmpeg_flags $placebo_flags -lm -lpthread -ldl
LD_LIBRARY_PATH="$out/lib" "$out/sources/abi" > "$out/sources/abi.json"
rm "$out/sources/abi"
LD_LIBRARY_PATH="$out/lib" ldd "$out/bin/segment_decode_render" \
    | sed -E 's/ \(0x[0-9a-f]+\)//g' > "$out/sources/runtime-dependencies.txt"
! grep -Eq 'not found|libav(codec|format|util)\.so' "$out/sources/runtime-dependencies.txt"
for archive in ffmpeg-source.tar.gz dovi-source.tar.gz libplacebo-source.tar.gz vulkan-headers-source.tar.gz; do
    cp "$deps/sources/$archive" "$out/sources/$archive"
done
cp "$deps/include/libdovi/rpu_parser.h" "$out/sources/rpu_parser.h"
cp "$deps/sources/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/Cargo.lock" "$out/sources/resolved-Cargo.lock"
cp -R "$deps/ffmpeg/lib" "$out/sources/ffmpeg-static-libraries"
cp "$source_dir"/*.c "$source_dir"/*.h "$source_dir"/*.sh "$source_dir"/bundle-manifest.py "$out/sources/"
cp "$source_dir/build-requirements.txt" "$out/sources/build-requirements.txt"
inputs=${PLURX_DV_BUILD_INPUTS:-"$source_dir/../dv_quality/backends"}
mkdir -p "$out/sources/build-inputs/parsed-rpu" "$out/sources/build-inputs/libplacebo"
cp "$inputs/parsed-rpu/fetch_parser.py" "$inputs/parsed-rpu/resolved-Cargo.lock" "$out/sources/build-inputs/parsed-rpu/"
cp "$inputs/libplacebo/fetch_sources.py" "$out/sources/build-inputs/libplacebo/"
cp -R "$deps/licenses/rust-crates" "$out/licenses/"
for component in "FFmpeg-bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa" "dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1" "libplacebo-0d043c7f6f79cd3687c023454bdacbe615e4d96f" "Vulkan-Headers-74d8a6cb930c68ef617b202c3ff3c59d919e086b"; do
    mkdir -p "$out/licenses/$component"
    find "$deps/sources/$component" -maxdepth 1 -type f \
        \( -iname 'license*' -o -iname 'copying*' \) -exec cp {} "$out/licenses/$component/" \;
done
for package in glslang-dev libxxhash-dev libvulkan-dev; do
    cp "/usr/share/doc/$package/copyright" "$out/licenses/$package-copyright"
done
python3 "$source_dir/bundle-manifest.py" "$source_dir" "$out"
