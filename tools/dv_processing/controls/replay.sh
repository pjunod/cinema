#!/bin/sh
# Replay the focused graph and metadata controls in a new scratch directory.
set -eu
[ "$#" = 3 ] || { echo 'usage: replay.sh FRESH_SCRATCH DEPENDENCY_ROOT DOVI_SOURCE' >&2; exit 2; }
work=$1
deps=$2
dovi=$3
[ ! -e "$work" ] || { echo 'refuse existing directory; use fresh scratch' >&2; exit 1; }
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$work/tools" "$work/generator/src" "$work/tags"
cp "$source_dir/../"*.c "$source_dir/../"*.h "$source_dir/../build.sh" "$work/tools/"
cp "$source_dir/"*.c "$source_dir/"*.py "$source_dir/"*.mkv "$work/"
cp "$source_dir/generator/Cargo.toml" "$source_dir/generator/Cargo.lock" "$work/generator/"
cp "$source_dir/generator/src/"*.rs "$work/generator/src/"
cp "$source_dir/tags/"*.nal "$work/tags/"
docker run --rm --cpus 2 --memory 2g \
  --mount "type=bind,src=$work,dst=/work" \
  --mount "type=bind,src=$deps/ffmpeg-prefix,dst=/work/ffmpeg-prefix,readonly" \
  --mount "type=bind,src=$deps/prefix,dst=/work/prefix,readonly" \
  --mount "type=bind,src=$deps/lib,dst=/work/lib,readonly" \
  --mount "type=bind,src=$deps/include,dst=/work/include,readonly" \
  --mount "type=bind,src=$dovi,dst=/dovi,readonly" \
  sha256:96951921bc396f9fb96e579d9c15b83abe8b488390f8a7851c3754b2d96114c9 \
  sh -c '
    set -eu
    export PYTHONDONTWRITEBYTECODE=1
    rustc --version
    cargo run --locked --release --manifest-path /work/generator/Cargo.toml -- /work
    sh /work/tools/build.sh /work/ffmpeg-prefix /work/prefix/lib/aarch64-linux-gnu/pkgconfig /work /work/bin
    flags=$(PKG_CONFIG_PATH=/work/ffmpeg-prefix/lib/pkgconfig pkg-config --static --cflags --libs libavformat libavcodec libavutil)
    cc -std=c11 -Wall -Wextra -Werror /work/make_movie.c -o /work/make_movie $flags -lm -lpthread -ldl
    cc -std=c11 -Wall -Wextra -Werror /work/make_long_terminal.c -o /work/make_long_terminal $flags -lm -lpthread -ldl
    cc -std=c11 -Wall -Wextra -Werror -I/work/include /work/test_fmp4_grid.c /work/lib/libdovi.a -o /work/test_fmp4_grid $flags -lm -lpthread -ldl
    for case in valid plain invalid-area nonzero-eotf binding-max long-l8; do
      /work/make_movie /work/bl.mkv /work/el.mkv /work/$case/rpus /work/$case/source.mkv normal > /work/$case/mux.jsonl
    done
    /work/make_long_terminal /work/bl.mkv /work/el.mkv /work/valid/rpus /work/long-terminal-unpatched.mkv normal > /work/long-terminal-mux.jsonl
    python3 /work/run_controls.py
    /work/generator/target/release/segment-guard-controls /work parse
    python3 /work/check_metadata.py /work
  '
