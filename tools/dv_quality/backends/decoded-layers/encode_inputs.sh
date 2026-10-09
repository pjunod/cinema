#!/bin/sh
set -eu
for bframes in 0 2; do
    for layer in bl el; do
        ffmpeg -nostdin -v warning -n -f rawvideo -pixel_format yuv420p10le -video_size 64x64 -framerate 24 -i /work/$layer.yuv420p10le -frames:v 6 -c:v libx265 -preset ultrafast -x265-params "lossless=1:bframes=$bframes:b-adapt=0:keyint=24:min-keyint=24:scenecut=0:aud=1:repeat-headers=1:chromaloc=1:colorprim=9:transfer=16:colormatrix=9:pools=1:frame-threads=1" -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv -chroma_sample_location center /work/$layer-b$bframes.mkv
        ffprobe -v error -show_packets -show_frames -show_streams -of json /work/$layer-b$bframes.mkv > /work/$layer-b$bframes.ffprobe.json
    done
done
