#!/bin/sh
set -eu
export PYTHONDONTWRITEBYTECODE=1
[ "$#" -eq 3 ] || { echo 'usage: replay.sh NEW-scratch approved-input-replay verified-offline-Cargo-registry' >&2; exit 2; }
bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$1" in /*) ;; *) exit 2 ;; esac
[ ! -e "$1" ] && [ ! -L "$1" ] || exit 2
scratch=$(python3 -B -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
case "$scratch" in "$bundle"|"$bundle"/*) exit 2 ;; esac
image=$(python3 -B "$bundle/image_identity.py" read "$2/image-id.txt")
mkdir "$scratch"
export TMPDIR="$scratch"
printf '%s\n' "$image" > "$scratch/image-id.txt"
python3 -B "$bundle/prepare_replay.py" "$scratch" "$2" "$3" > "$scratch/dependencies.json"
docker run --rm --network none --cpus 2 --memory 2g --env PYTHONDONTWRITEBYTECODE=1 --mount "type=bind,source=$scratch,target=/work" "$image" sh /work/build_ffmpeg.sh > "$scratch/build-ffmpeg.log" 2>&1
docker run --rm --network none --cpus 2 --memory 2g --env PYTHONDONTWRITEBYTECODE=1 --env CARGO_HOME=/work/cargo-home --mount "type=bind,source=$scratch,target=/work" "$image" sh -ec 'rustc --version | tee /work/rust-baseline-version.txt; grep "^rustc 1.97.1 " /work/rust-baseline-version.txt; cargo check --offline --locked --manifest-path /work/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/Cargo.toml -p dolby_vision --features serde; sh /work/generate.sh; sh /work/run_controls.sh' > "$scratch/control-run.log" 2>&1
python3 -B "$bundle/check_reuse.py" "$scratch" > "$scratch/results.json"
python3 -B "$bundle/test_reuse.py" "$scratch" > "$scratch/checker-tests.log" 2>&1
docker run --rm --network none --cpus 2 --memory 2g --ulimit core=0 --env PYTHONDONTWRITEBYTECODE=1 --mount "type=bind,source=$scratch,target=/work" "$image" python3 /work/test_driver.py /work > "$scratch/driver-tests.log" 2>&1
