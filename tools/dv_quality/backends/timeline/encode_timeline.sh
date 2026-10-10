#!/bin/sh
set -eu
cd /work
schedule="if(eq(N,0),0,if(eq(N,1),40,if(eq(N,2),110,if(eq(N,3),140,if(eq(N,4),230,300)))))"
for layer in bl el; do
  for b in 0 2; do
    ffmpeg -nostdin -n -v error -f rawvideo -pixel_format yuv420p10le -video_size 64x64 -framerate 24 -i $layer.yuv420p10le -vf "settb=1/1000,setpts='$schedule'" -vsync 0 -enc_time_base 1/1000 -c:v libx265 -pix_fmt yuv420p10le -x265-params "lossless=1:bframes=$b:b-adapt=0:keyint=3:min-keyint=3:scenecut=0:open-gop=0:aud=1:repeat-headers=1:chromaloc=1:pools=1:frame-threads=1" -color_range tv -colorspace bt2020nc -color_trc smpte2084 -color_primaries bt2020 -frames:v 6 "$layer-vfr-b$b.mkv" 2> "$layer-vfr-b$b.encoder.log"
    ffprobe -v error -show_streams -show_packets -show_frames -of json "$layer-vfr-b$b.mkv" > "$layer-vfr-b$b.ffprobe.json"
  done
done
for segment in 0 1; do
  for layer in bl el; do
    ffmpeg -nostdin -n -v error -f rawvideo -pixel_format yuv420p10le -video_size 64x64 -framerate 24 -i "$layer-segment$segment.yuv420p10le" -vf "settb=1/1000,setpts='if(eq(N,0),0,if(eq(N,1),40,110))'" -vsync 0 -enc_time_base 1/1000 -c:v libx265 -pix_fmt yuv420p10le -x265-params "lossless=1:bframes=2:b-adapt=0:keyint=3:min-keyint=3:scenecut=0:open-gop=0:aud=1:repeat-headers=1:chromaloc=1:pools=1:frame-threads=1" -color_range tv -colorspace bt2020nc -color_trc smpte2084 -color_primaries bt2020 -frames:v 3 "$layer-segment$segment.mkv" 2> "$layer-segment$segment.encoder.log"
    ffprobe -v error -show_streams -show_packets -show_frames -of json "$layer-segment$segment.mkv" > "$layer-segment$segment.ffprobe.json"
  done
done
