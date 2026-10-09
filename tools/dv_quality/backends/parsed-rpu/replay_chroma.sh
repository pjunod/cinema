#!/bin/sh
set -eu
[ "$#" -eq 2 ] || { echo 'usage: replay_chroma.sh NEW-scratch prerequisite-replay' >&2; exit 2; }
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
image_id=$(python3 "$bundle_dir/image_identity.py" read "$2/image-id.txt")
case "$1" in /*) ;; *) exit 2 ;; esac
[ ! -e "$1" ] && [ ! -L "$1" ] || exit 2
scratch_dir=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
case "$scratch_dir" in "$bundle_dir"|"$bundle_dir"/*) exit 2 ;; esac
mkdir "$scratch_dir"
printf '%s\n' "$image_id" > "$scratch_dir/image-id.txt"
python3 "$bundle_dir/chroma_control.py" generate "$scratch_dir"
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh -ec '
    for location in left center; do
        ffmpeg -nostdin -v error -n -f rawvideo -pixel_format yuv444p10le -video_size 16x16 -i /work/native444.yuv -vf "zscale=matrixin=2020_ncl:primariesin=2020:transferin=smpte2084:rangein=limited:chromalin=center:matrix=2020_ncl:primaries=2020:transfer=smpte2084:range=limited:chromal=$location:filter=point,format=yuv420p10le" -frames:v 1 -f rawvideo /work/native-$location.yuv
        ffmpeg -nostdin -v error -n -f rawvideo -pixel_format rgb48le -video_size 16x16 -i /work/rgb-edge.rgb48le -vf "zscale=matrixin=gbr:primariesin=2020:transferin=smpte2084:rangein=full:matrix=2020_ncl:primaries=2020:transfer=smpte2084:range=limited:chromal=$location:filter=point,format=yuv420p10le" -frames:v 1 -f rawvideo /work/rgb-$location.yuv
    done
' > "$scratch_dir/chroma-run.log" 2>&1
python3 "$bundle_dir/chroma_control.py" check "$scratch_dir" > "$scratch_dir/chroma-results.json"
python3 "$bundle_dir/image_identity.py" record "$scratch_dir" chroma "$image_id"
