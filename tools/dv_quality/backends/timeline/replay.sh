#!/bin/sh
set -eu
export PYTHONDONTWRITEBYTECODE=1
[ "$#" -eq 3 ] || { echo 'usage: replay.sh NEW-scratch approved-decoded-replay approved-decoded-bundle' >&2; exit 2; }
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
python3 "$bundle_dir/dependency_identity.py" "$2" "$3" > /dev/null
image_id=$(python3 "$bundle_dir/image_identity.py" read "$2/image-id.txt")
case "$1" in /*) ;; *) exit 2 ;; esac
[ ! -e "$1" ] && [ ! -L "$1" ] || exit 2
scratch_dir=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
case "$scratch_dir" in "$bundle_dir"|"$bundle_dir"/*) exit 2 ;; esac
mkdir "$scratch_dir"
export TMPDIR="$scratch_dir"
printf '%s\n' "$image_id" > "$scratch_dir/image-id.txt"
python3 "$bundle_dir/dependency_identity.py" "$2" "$3" > "$scratch_dir/dependencies-before-copy.json"
cp -R "$2/ffmpeg-prefix" "$scratch_dir/ffmpeg-prefix"
cp -R "$3/rpu-tags" "$scratch_dir/rpu-tags"
python3 "$bundle_dir/dependency_identity.py" "$scratch_dir" > "$scratch_dir/dependencies-after-copy.json"
for filename in make_timeline_inputs.py mux_timeline.c decode_timeline.c encode_timeline.sh run_timeline.sh decode_cases.sh test_driver.py check_timeline.py inspect_matroska.py; do cp "$bundle_dir/$filename" "$scratch_dir/$filename"; done
python3 "$scratch_dir/make_timeline_inputs.py" "$scratch_dir"
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh /work/encode_timeline.sh > "$scratch_dir/encode.log" 2>&1
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh /work/run_timeline.sh > "$scratch_dir/run.log" 2>&1
python3 "$bundle_dir/check_timeline.py" "$scratch_dir" > "$scratch_dir/results.json"
python3 "$bundle_dir/test_timeline.py" "$scratch_dir" > "$scratch_dir/timeline-tests.log" 2>&1
docker run --rm --network none --cpus 2 --memory 2g --ulimit core=0 --env PYTHONDONTWRITEBYTECODE=1 --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" python3 /work/test_driver.py /work > "$scratch_dir/driver-tests.log" 2>&1
