#!/usr/bin/env bash
set -euo pipefail
# These are the exact successful preparation stages used on 2026-10-20.
# HB_FFMPEG was /opt/homebrew/bin/ffmpeg (9.0.2).
# JELLYFIN_FFMPEG was the installed Jellyfin 8.1.3-Jellyfin executable.
: "${HB_FFMPEG:?Set the FFmpeg 9.0.2 executable}"
: "${JELLYFIN_FFMPEG:?Set the Jellyfin FFmpeg 8.1.3 executable}"
: "${FIXTURE_WORK:?Set an empty scratch directory for preparation}"
mkdir -p "$FIXTURE_WORK"
cd "$FIXTURE_WORK"
"$HB_FFMPEG" -hide_banner -loglevel error -rw_timeout 20000000 -ss 60 -i 'https://s3.amazonaws.com/download.opencontent.netflix.com/SolLevante/hdr10/SolLevante_HDR10_r2020_ST2084_UHD_24fps_1000nit.mov' -map 0:v:0 -an -frames:v 240 -c:v libx265 -preset fast -crf 28 -pix_fmt yuv420p10le -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -x265-params 'pools=4:frame-threads=2:hdr10=1:repeat-headers=1' -y source-240-hdr10.mkv
"$JELLYFIN_FFMPEG" -hide_banner -loglevel error -i source-240-hdr10.mkv -an -vf 'zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p,scale=1920:1080' -frames:v 240 -c:v libx264 -preset fast -crf 24 -threads 4 -color_primaries bt709 -color_trc bt709 -colorspace bt709 -y 1080p-h264-sdr.mkv
"$JELLYFIN_FFMPEG" -hide_banner -loglevel error -i source-240-hdr10.mkv -an -vf 'zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p10le' -frames:v 240 -c:v libx265 -preset fast -crf 28 -x265-params 'pools=4:frame-threads=2' -color_primaries bt709 -color_trc bt709 -colorspace bt709 -y 4k-hevc-sdr.mkv
"$HB_FFMPEG" -hide_banner -loglevel error -i source-240-hdr10.mkv -an -frames:v 240 -c:v libx265 -preset fast -b:v 8M -maxrate 10M -bufsize 16M -x265-params 'pools=4:frame-threads=2:hdr10=1:repeat-headers=1' -pix_fmt yuv420p10le -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -y 4k-hevc-hdr10-final.mkv
"$HB_FFMPEG" -hide_banner -loglevel error -i 4k-hevc-sdr.mkv -an -frames:v 240 -c:v libx265 -preset fast -b:v 8M -maxrate 10M -bufsize 16M -x265-params 'pools=4:frame-threads=2' -pix_fmt yuv420p10le -color_primaries bt709 -color_trc bt709 -colorspace bt709 -y 4k-hevc-sdr-final.mkv
"$HB_FFMPEG" -hide_banner -loglevel error -i 1080p-h264-sdr.mkv -an -frames:v 240 -c:v libx264 -preset fast -b:v 2M -maxrate 3M -bufsize 4M -threads 4 -pix_fmt yuv420p -color_primaries bt709 -color_trc bt709 -colorspace bt709 -y 1080p-h264-sdr-final.mkv

# The final names are the runtime fixture names. Do not overwrite the committed
# copies automatically: verify frame/color metadata and review the new hashes.
cp 1080p-h264-sdr-final.mkv 1080p-h264-sdr-release.mkv
cp 4k-hevc-sdr-final.mkv 4k-hevc-sdr-release.mkv
cp 4k-hevc-hdr10-final.mkv 4k-hevc-hdr10-release.mkv
