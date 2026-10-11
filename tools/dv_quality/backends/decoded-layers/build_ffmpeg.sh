#!/bin/sh
set -eu
cd /work/FFmpeg-bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa
./configure --prefix=/work/ffmpeg-prefix --disable-autodetect --disable-doc --disable-debug --disable-asm --disable-programs --disable-everything --disable-network --disable-avdevice --disable-avfilter --disable-swresample --disable-swscale --enable-static --disable-shared --enable-avcodec --enable-avformat --enable-avutil --enable-decoder=hevc --enable-parser=hevc --enable-bsf=dovi_split,hevc_mp4toannexb,extract_extradata --enable-demuxer=matroska,hevc --enable-muxer=matroska --enable-protocol=file
make -j2
make install
