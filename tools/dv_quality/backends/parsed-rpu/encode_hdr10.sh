#!/bin/sh
# Operates only on an already-created cross replay scratch directory.
set -eu
[ "$#" -eq 1 ] || exit 2
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
image_id=$(python3 "$bundle_dir/image_identity.py" read "$1/image-id.txt")
[ -f "$1/parsed-p81-four.rgb48le" ] || exit 2
[ ! -e "$1/parsed-render-hdr10.hevc" ] || exit 2
scratch_dir=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh -ec '
    ffmpeg -nostdin -hide_banner -n -f rawvideo -pixel_format rgb48le -video_size 64x64 -framerate 24 -color_primaries bt2020 -color_trc smpte2084 -colorspace rgb -color_range pc -i /work/parsed-p81-four.rgb48le -vf "zscale=matrixin=gbr:primariesin=2020:transferin=smpte2084:rangein=full:matrix=2020_ncl:primaries=2020:transfer=smpte2084:range=limited:chromal=center:filter=point,format=yuv420p10le" -frames:v 4 -c:v libx265 -preset ultrafast -x265-params "qp=0:bframes=0:keyint=24:scenecut=0:repeat-headers=1:chromaloc=1:colorprim=9:transfer=16:colormatrix=9:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(100000000,50):pools=1:frame-threads=1" -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv /work/parsed-render-hdr10.hevc
    ffprobe -v error -show_streams -show_frames -of json /work/parsed-render-hdr10.hevc > /work/parsed-render-hdr10.ffprobe.json
' > "$scratch_dir/hdr10-encode.log" 2>&1
python3 "$bundle_dir/check_encoded.py" "$scratch_dir" > "$scratch_dir/encode-results.json"
python3 "$bundle_dir/image_identity.py" record "$scratch_dir" encode "$image_id"
