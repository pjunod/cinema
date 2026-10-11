#!/bin/sh
# Source-only bridge replay. Prerequisite: approved libplacebo renderer replay.
set -eu
[ "$#" -eq 2 ] || { echo 'usage: replay_parsed.sh NEW-scratch approved-libplacebo-replay' >&2; exit 2; }
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
image_id=$(python3 "$bundle_dir/image_identity.py" read "$2/image-id.txt")
case "$1" in /*) ;; *) exit 2 ;; esac
[ ! -e "$1" ] && [ ! -L "$1" ] || { echo 'scratch must not exist' >&2; exit 2; }
scratch_dir=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
case "$scratch_dir" in "$bundle_dir"|"$bundle_dir"/*) exit 2 ;; esac
mkdir "$scratch_dir"
printf '%s\n' "$image_id" > "$scratch_dir/image-id.txt"
cp -R "$2/prefix" "$scratch_dir/prefix"
cp -R "$bundle_dir/fixtures" "$scratch_dir/fixtures"
cp "$bundle_dir/parsed_export_probe.c" "$scratch_dir/parsed_export_probe.c"
python3 "$bundle_dir/fetch_parser.py" "$scratch_dir"
# Rust is pinned and verified; Cargo.lock includes registry content checksums.
# The existing mechanics image identity is recorded. Rebuilding apt/pip may drift.
docker run --rm --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh -ec '
    rustc --version | grep "rustc 1.97.1 "
    cargo rustc --locked --release --manifest-path /work/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/Cargo.toml -p dolby_vision --features dolby_vision/capi --crate-type staticlib
    mkdir /work/lib
    cp /work/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/target/release/libdolby_vision.a /work/lib/libdovi.a
' > "$scratch_dir/parser-build.log" 2>&1
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh -ec '
    export PKG_CONFIG_PATH=/work/prefix/lib/aarch64-linux-gnu/pkgconfig
    export LD_LIBRARY_PATH=/work/prefix/lib/aarch64-linux-gnu
    export XDG_RUNTIME_DIR=/tmp/runtime-m0 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json
    mkdir "$XDG_RUNTIME_DIR" /work/outputs; chmod 700 "$XDG_RUNTIME_DIR"
    cc -Wall -Wextra -Werror -std=c11 -I/work/include /work/parsed_export_probe.c /work/lib/libdovi.a -o /work/parsed_export_probe $(pkg-config --cflags --libs libplacebo) -lm -lpthread -ldl
    cd /work
    ./parsed_export_probe fixtures/p7_fel_linear.nal > parsed-probe.jsonl 2> parsed-probe.stderr
    for control in missing truncated bounded missing-el; do
        case "$control" in
            missing) rpu=/work/fixtures/missing.nal; mode= ;;
            truncated) rpu=/work/fixtures/truncated.nal; mode= ;;
            bounded) rpu=/work/fixtures/p7_fel_bounded_residual.nal; mode= ;;
            missing-el) rpu=/work/fixtures/p7_fel_linear.nal; mode=require-missing-el ;;
        esac
        mkdir -p /work/negatives/$control/outputs
        cd /work/negatives/$control
        set +e
        /work/parsed_export_probe "$rpu" $mode > stdout.txt 2> stderr.txt
        status=$?
        set -e
        printf "%s\n" "$status" > status.txt
        printf "%s %s\n" "$control" "$status" >> /work/negative-status.txt
        [ "$status" -eq 1 ] || exit 65
    done
' > "$scratch_dir/parser-render.log" 2>&1
python3 "$bundle_dir/check_negatives.py" "$scratch_dir" > "$scratch_dir/negative-results.json"
python3 "$bundle_dir/check_parsed.py" "$scratch_dir" > "$scratch_dir/check-results.json"
python3 "$bundle_dir/image_identity.py" record "$scratch_dir" parsed "$image_id"
