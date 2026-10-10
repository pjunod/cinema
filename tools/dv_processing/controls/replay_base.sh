#!/bin/sh
# Focused base-only controls, using existing encoded BL/EL and locked public generator.
set -eu
[ "$#" = 3 ] || { echo 'usage: replay_base.sh FRESH_SCRATCH DEPENDENCY_ROOT DOVI_SOURCE' >&2; exit 2; }
work=$1
deps=$2
dovi=$3
[ ! -e "$work" ] || { echo 'refuse existing directory; use fresh scratch' >&2; exit 1; }
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$work/tools" "$work/generator/src" "$work/tags"
cp "$source_dir/../"*.c "$source_dir/../"*.h "$source_dir/../build.sh" "$work/tools/"
cp "$source_dir/make_movie.c" "$source_dir/run_base_controls.py" "$source_dir/"*.mkv "$work/"
cp "$source_dir/generator/Cargo.toml" "$source_dir/generator/Cargo.lock" "$work/generator/"
cp "$source_dir/generator/src/main.rs" "$source_dir/generator/src/base_profiles.rs" "$work/generator/src/"
cp "$source_dir/tags/"*.nal "$work/tags/"
image=${DV_TEST_IMAGE:-sha256:96951921bc396f9fb96e579d9c15b83abe8b488390f8a7851c3754b2d96114c9}
arch=${DV_TEST_ARCH_LIB:-aarch64-linux-gnu}
icd=${DV_TEST_VULKAN_ICD:-/usr/share/vulkan/icd.d/lvp_icd.aarch64.json}
case "$arch" in aarch64-linux-gnu|x86_64-linux-gnu) ;; *) echo 'unsupported test architecture' >&2; exit 1;; esac
set -- --rm --cpus 2 --memory 2g \
  --mount "type=bind,src=$work,dst=/work" \
  --mount "type=bind,src=$deps/ffmpeg-prefix,dst=/work/ffmpeg-prefix,readonly" \
  --mount "type=bind,src=$deps/prefix,dst=/work/prefix,readonly" \
  --mount "type=bind,src=$deps/lib,dst=/work/lib,readonly" \
  --mount "type=bind,src=$deps/include,dst=/work/include,readonly" \
  --mount "type=bind,src=$dovi,dst=/dovi,readonly" \
  -e "DV_TEST_ARCH_LIB=$arch" -e "DV_TEST_VULKAN_ICD=$icd"
# A public registry cache is optional; never mount a credential-bearing Cargo home.
if [ -n "${DV_TEST_REGISTRY_DIR:-}" ]; then
  set -- "$@" --network none --mount "type=bind,src=$DV_TEST_REGISTRY_DIR,dst=/public-registry,readonly" -e DV_TEST_OFFLINE=1
fi
docker run "$@" "$image" sh -c '
  set -eu
  export PYTHONDONTWRITEBYTECODE=1
  rustc --version | grep "^rustc 1.97.1 "
  if [ "${DV_TEST_OFFLINE:-}" = 1 ]; then
    mkdir /work/cargo-home
    ln -s /public-registry /work/cargo-home/registry
    export CARGO_HOME=/work/cargo-home CARGO_NET_OFFLINE=true
  fi
  cargo run --locked --release --manifest-path /work/generator/Cargo.toml -- /work
  cargo run --locked --release --manifest-path /work/generator/Cargo.toml -- /work base
  sh /work/tools/build.sh /work/ffmpeg-prefix /work/prefix/lib/$DV_TEST_ARCH_LIB/pkgconfig /work /work/bin
  flags=$(PKG_CONFIG_PATH=/work/ffmpeg-prefix/lib/pkgconfig pkg-config --static --cflags --libs libavformat libavcodec libavutil)
  cc -O2 -std=c11 -Wall -Wextra -Werror /work/make_movie.c -o /work/make_movie $flags -lm -lpthread -ldl
  python3 /work/run_base_controls.py
'
