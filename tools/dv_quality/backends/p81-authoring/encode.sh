#!/bin/bash
set -euo pipefail
exec > >(tee /work/encode.log) 2>&1
uname -a
ffmpeg -version
cp /opt/authoring-package-versions.txt /work/observed-package-versions.txt
python3 /work/prepare_frames.py
ffmpeg -y -f rawvideo -pixel_format rgb48le -video_size 64x64 -framerate 24 \
    -color_primaries bt2020 -color_trc smpte2084 -colorspace 0 -color_range pc \
    -i /work/reconstructed64.rgb48le \
    -vf 'format=gbrp16le,zscale=matrixin=gbr:matrix=2020_ncl:rangein=full:range=limited:primariesin=2020:primaries=2020:transferin=smpte2084:transfer=smpte2084:dither=none:filter=point:chromalin=center:chromal=center,format=yuv420p10le' \
    -c:v rawvideo -f rawvideo /work/reconstructed64.yuv420p10le
ffmpeg -y -f rawvideo -pixel_format yuv420p10le -video_size 64x64 -framerate 24 \
    -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv -chroma_sample_location center \
    -i /work/reconstructed64.yuv420p10le \
    -c:v libx265 -profile:v main10 -preset slow \
    -x265-params 'qp=0:pools=1:frame-threads=1:bframes=0:keyint=24:scenecut=0:repeat-headers=1:colorprim=9:transfer=16:colormatrix=9:range=limited:aud=1:chromaloc=1:hdr10=1:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(10000000,50):max-cll=1000,400' \
    -f hevc /work/hdr10-base.hevc
python3 /work/inject_rpu.py
ffmpeg -y -i /work/candidate.hevc -map 0:v:0 -c:v rawvideo -pix_fmt yuv420p10le -f rawvideo /work/decoded.yuv420p10le
ffmpeg -y -i /work/candidate.hevc \
    -vf 'zscale=matrixin=2020_ncl:matrix=gbr:rangein=limited:range=full:primariesin=2020:primaries=2020:transferin=smpte2084:transfer=smpte2084:dither=none:filter=point:chromalin=center:chromal=center,format=gbrp16le' \
    -c:v rawvideo -pix_fmt rgb48le -f rawvideo /work/decoded-hdr10.rgb48le
ffmpeg -y -i /work/hdr10-base.hevc -map 0:v:0 -c:v rawvideo -pix_fmt yuv420p10le -f rawvideo /work/baseline-decoded.yuv420p10le
ffprobe -v error -show_streams -show_frames -of json /work/candidate.hevc > /work/probe.json
