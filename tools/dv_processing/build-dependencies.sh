#!/bin/sh
# Source-only helper dependencies. Does not build or replace the media encoder.
set -eu
root=${1:?usage: build-dependencies.sh ABSOLUTE_PREFIX}
case "$root" in /*) ;; *) echo 'dependency prefix must be absolute' >&2; exit 2 ;; esac
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
inputs=${PLURX_DV_BUILD_INPUTS:-"$source_dir/../dv_quality/backends"}
jobs=${PLURX_DV_BUILD_JOBS:-2}
case "$jobs" in 1|2|3|4) ;; *) echo 'helper build jobs must be 1-4' >&2; exit 2 ;; esac
rustc --version | grep -q '^rustc 1\.97\.1 '
mkdir -p "$root/sources" "$root/include/libdovi" "$root/lib"
python3 "$inputs/parsed-rpu/fetch_parser.py" "$root/sources"
python3 "$inputs/libplacebo/fetch_sources.py" "$root/sources"
ffmpeg_revision=bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa
archive="$root/sources/ffmpeg-source.tar.gz"
curl --proto '=https' --tlsv1.2 --fail --location --retry 2 \
    "https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/$ffmpeg_revision" -o "$archive"
printf '%s  %s\n' fb1931fd4eb29297ee1c1017a24f800c4d8fbea35b4f2aaeb28308a48a9149b4 "$archive" | sha256sum -c -
tar -xzf "$archive" -C "$root/sources"
dovi="$root/sources/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1"
cargo rustc --locked --release --jobs "$jobs" --manifest-path "$dovi/Cargo.toml" \
    -p dolby_vision --features dolby_vision/capi --crate-type staticlib
cp "$dovi/target/release/libdolby_vision.a" "$root/lib/libdovi.a"
cp "$root/sources/include/libdovi/rpu_parser.h" "$root/include/libdovi/rpu_parser.h"
meson setup "$root/placebo-build" "$root/sources/libplacebo-0d043c7f6f79cd3687c023454bdacbe615e4d96f" \
    --prefix="$root/placebo" --buildtype=release -Dvulkan=enabled -Dopengl=disabled \
    -Dd3d11=disabled -Dglslang=enabled -Dshaderc=disabled -Ddovi=enabled \
    -Dlibdovi=disabled -Ddemos=false -Dtests=false -Dxxhash=enabled
meson compile -C "$root/placebo-build" -j "$jobs"
meson install -C "$root/placebo-build"
mkdir "$root/ffmpeg-build"
cd "$root/ffmpeg-build"
"$root/sources/FFmpeg-$ffmpeg_revision/configure" --prefix="$root/ffmpeg" \
    --disable-autodetect --disable-doc --disable-debug --disable-programs \
    --disable-everything --disable-network --disable-avdevice --disable-avfilter \
    --disable-swresample --disable-swscale --enable-static --disable-shared \
    --enable-avcodec --enable-avformat --enable-avutil --enable-decoder=hevc \
    --enable-parser=hevc --enable-bsf=dovi_split,hevc_mp4toannexb,extract_extradata \
    --enable-demuxer=matroska,hevc,mov --enable-muxer=matroska,nut,mp4 --enable-protocol=file
make -j "$jobs"
make install

# Preserve the license texts of fetched Rust code linked into libdovi.
mkdir -p "$root/licenses/rust-crates"
registry=${CARGO_HOME:-"$HOME/.cargo"}/registry/src
find "$registry" -mindepth 2 -maxdepth 2 -type d | while IFS= read -r package; do
    name=$(basename "$package")
    mkdir -p "$root/licenses/rust-crates/$name"
    find "$package" -maxdepth 1 -type f \
        \( -iname 'license*' -o -iname 'copying*' -o -iname 'notice*' \) \
        -exec cp {} "$root/licenses/rust-crates/$name/" \;
done
