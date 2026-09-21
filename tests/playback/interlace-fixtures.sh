#!/usr/bin/env bash
set -euo pipefail

: "${PLURX_FFMPEG:?set PLURX_FFMPEG to the shipped ffmpeg executable}"
PLURX_FFPROBE="${PLURX_FFPROBE:-ffprobe}"
work="$(mktemp -d "${TMPDIR:-/tmp}/plurx-interlace.XXXXXX")"
trap 'rm -rf "$work"' EXIT

make_interlaced() {
  name="$1" size="$2" source_rate="$3" interlace="$4"
  "$PLURX_FFMPEG" -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=${size}:rate=${source_rate}:duration=2" \
    -vf "tinterlace=mode=${interlace},setfield=tff" -c:v ffv1 "$work/${name}.mkv"
}

verify_bwdif() {
  name="$1"
  input_rate="$($PLURX_FFPROBE -v error -select_streams v:0 \
    -show_entries stream=avg_frame_rate -of default=nw=1:nk=1 "$work/${name}.mkv")"
  "$PLURX_FFMPEG" -hide_banner -loglevel error -y -i "$work/${name}.mkv" \
    -vf "bwdif=mode=send_frame:parity=auto:deint=interlaced,scale=-2:'min(1080,ih)'" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p "$work/${name}-out.mkv"
  output_rate="$($PLURX_FFPROBE -v error -select_streams v:0 \
    -show_entries stream=avg_frame_rate -of default=nw=1:nk=1 "$work/${name}-out.mkv")"
  test "$input_rate" = "$output_rate"

  summary="$($PLURX_FFMPEG -hide_banner -i "$work/${name}-out.mkv" \
    -vf idet -an -f null - 2>&1 | grep 'Multi frame detection' | tail -1)"
  counts="$(printf '%s\n' "$summary" | sed -E \
    's/.*TFF:[[:space:]]*([0-9]+).*BFF:[[:space:]]*([0-9]+).*Progressive:[[:space:]]*([0-9]+).*/\1 \2 \3/')"
  read -r tff bff progressive <<<"$counts"
  total=$((tff + bff + progressive))
  test "$total" -gt 0
  test $((progressive * 100)) -ge $((total * 95))
}

make_interlaced tff-480 720x480 60000/1001 interleave_top
make_interlaced tff-576 720x576 50 interleave_top
make_interlaced tff-1080 1920x1080 60000/1001 interleave_top
make_interlaced bff-480 720x480 60000/1001 interleave_bottom
for fixture in tff-480 tff-576 tff-1080 bff-480; do verify_bwdif "$fixture"; done

# The progressive control never receives bwdif in the production plan, so the
# pre-S-08 and S-08 command are deliberately the same bytes.
"$PLURX_FFMPEG" -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=1920x1080:rate=60000/1001:duration=2" \
  -vf "scale=-2:'min(1080,ih)'" -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
  "$work/progressive-before.mkv"
"$PLURX_FFMPEG" -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=1920x1080:rate=60000/1001:duration=2" \
  -vf "scale=-2:'min(1080,ih)'" -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
  "$work/progressive-after.mkv"
cmp "$work/progressive-before.mkv" "$work/progressive-after.mkv"

# Hard telecine is retained as a real input fixture for manual inspection.
"$PLURX_FFMPEG" -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=720x480:rate=24000/1001:duration=2" \
  -vf "telecine=pattern=32" -c:v ffv1 "$work/hard-telecine.mkv"

# A field-order tag alone must not make genuinely progressive frames visibly
# diverge. Compare bwdif's output with the same progressive reference.
"$PLURX_FFMPEG" -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=640x360:rate=30000/1001:duration=2" \
  -vf "setfield=tff" -c:v ffv1 "$work/misflagged.mkv"
psnr="$($PLURX_FFMPEG -hide_banner -i "$work/misflagged.mkv" \
  -filter_complex "[0:v]split[plain][marked];[marked]bwdif=mode=send_frame:parity=auto:deint=interlaced[filtered];[plain][filtered]psnr" \
  -an -f null - 2>&1 | grep 'average:' | tail -1 | sed -E 's/.*average:([^ ]+).*/\1/')"
awk -v value="$psnr" 'BEGIN { exit !(value == "inf" || value + 0 >= 40) }'

