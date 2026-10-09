#!/bin/sh
set -eu
export PYTHONDONTWRITEBYTECODE=1
[ "$#" -eq 3 ] || { echo 'usage: replay.sh NEW-scratch approved-parsed-renderer-replay reviewed-parsed-bundle' >&2; exit 2; }
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
python3 "$bundle_dir/prerequisite_identity.py" "$2" "$3" > /dev/null
image_id=$(python3 "$bundle_dir/image_identity.py" read "$2/image-id.txt")
case "$1" in /*) ;; *) exit 2 ;; esac
[ ! -e "$1" ] && [ ! -L "$1" ] || exit 2
scratch_dir=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
case "$scratch_dir" in "$bundle_dir"|"$bundle_dir"/*) exit 2 ;; esac
mkdir "$scratch_dir"
export TMPDIR="$scratch_dir"
printf '%s\n' "$image_id" > "$scratch_dir/image-id.txt"
python3 "$bundle_dir/prerequisite_identity.py" "$2" "$3" > "$scratch_dir/prerequisite-before-copy.json"
python3 "$bundle_dir/test_prerequisite.py" "$2" "$3" > "$scratch_dir/prerequisite-tests.log" 2>&1
for directory in prefix include lib; do cp -R "$2/$directory" "$scratch_dir/$directory"; done
python3 "$bundle_dir/prerequisite_identity.py" "$scratch_dir" > "$scratch_dir/prerequisite-after-copy.json"
cp -R "$bundle_dir/rpu-tags" "$scratch_dir/rpu-tags"
for filename in make_inputs.py encode_inputs.sh build_ffmpeg.sh mux_layers.c decode_layers.c run_decode.sh check_association.py decoded_export_probe.c image_identity.py; do cp "$bundle_dir/$filename" "$scratch_dir/$filename"; done
python3 "$bundle_dir/fetch_ffmpeg.py" "$scratch_dir"
python3 "$scratch_dir/make_inputs.py"
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh /work/build_ffmpeg.sh > "$scratch_dir/ffmpeg-build.log" 2>&1
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh /work/encode_inputs.sh > "$scratch_dir/encode-inputs.log" 2>&1
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh /work/run_decode.sh > "$scratch_dir/decode-run.log" 2>&1
python3 "$bundle_dir/check_association.py" "$scratch_dir" > "$scratch_dir/association-results.json"
python3 "$bundle_dir/test_association.py" "$scratch_dir" > "$scratch_dir/association-tests.log" 2>&1
python3 "$bundle_dir/render_accepted.py" "$scratch_dir"
python3 "$bundle_dir/check_rendered.py" "$scratch_dir" > "$scratch_dir/render-results.json"
python3 "$bundle_dir/test_rendered.py" "$scratch_dir" > "$scratch_dir/rendered-tests.log" 2>&1
python3 "$bundle_dir/image_identity.py" record "$scratch_dir" parsed "$image_id"
