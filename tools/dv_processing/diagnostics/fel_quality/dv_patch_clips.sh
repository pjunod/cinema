#!/bin/sh
# dv_patch_clips.sh READBACK.json SRC_P7_RPU.nal WORKDIR -- build and render the dv_patch_check.py flat-patch clips on macOS.
# Needs: ffmpeg with libx265, MP4Box (gpac), python3 with numpy, the rpu_refit_write binary, swiftc.
# Env: FFMPEG MP4BOX PY WRITER (defaults: on PATH / ./rpu_refit_write/target/release/rpu_refit_write).
set -e
RB=$(cd "$(dirname "$1")" && pwd)/$(basename "$1"); SRC=$(cd "$(dirname "$2")" && pwd)/$(basename "$2"); D=$(cd "$(dirname "$0")" && pwd)
FFMPEG=${FFMPEG:-ffmpeg}; MP4BOX=${MP4BOX:-MP4Box}; PY=${PY:-python3}; WRITER=${WRITER:-$D/rpu_refit_write/target/release/rpu_refit_write}
mkdir -p "$3" && cd "$3"
$PY "$D/dv_patch_check.py" make "$RB" .
# 96 frames of the same RPU: the re-fit curve (from READBACK) and the identity curve, both converted from one source P7 RPU
$PY - "$RB" "$SRC" <<'PY'
import json, sys
from pathlib import Path
r0 = json.loads(Path(sys.argv[1]).read_text())['frames'][0]
piv = r0['luma_pivot_deltas']
def ch(k):
    c = [[0.0] * 7 for _ in range(3)]; c[0][k] = 1.0; return {'constant': 0.0, 'coef': c}
for name, luma, chroma in (('refit', {'pivot_deltas': piv, 'coef': r0['luma_coef']}, r0['chroma']),
                           ('ident', {'pivot_deltas': [0, 1023], 'coef': [[0.0, 1.0, 0.0]]}, [ch(1), ch(2)])):
    Path(f'nal-{name}').mkdir(exist_ok=True)
    Path(f'plan-{name}.json').write_text(json.dumps({'frames': [{'src': sys.argv[2], 'out': f'nal-{name}/frame-{i:03d}.nal'} for i in range(96)],
                                                     'luma': luma, 'chroma': chroma}))
PY
for v in refit ident; do "$WRITER" plan-$v.json > readback-$v.json; done
$PY -c "
import json, sys
a = json.load(open('readback-refit.json'))['frames'][0]; b = json.load(open('$RB'))['frames'][0]
assert (a['luma_coef'], a['chroma'], a['luma_pivot_deltas']) == (b['luma_coef'], b['chroma'], b['luma_pivot_deltas']), 'rewritten curve differs'"
for p in A B; do
  "$FFMPEG" -nostdin -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p10le -s 3840x2160 -r 24000/1001 -stream_loop 95 -i $p.yuv \
    -frames:v 96 -c:v libx265 -x265-params "lossless=1:bframes=0:keyint=24:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:log-level=error" -f hevc $p.hevc
done
for c in "A refit A" "B ident B" "A ident C"; do set -- $c
  $PY "$D/inject_rpu.py" $1.hevc nal-$2 $3-dv.hevc > /dev/null
  rm -f $3.mp4; "$MP4BOX" -add $3-dv.hevc:dvp=8.hdr10:fps=24000/1001 -new $3.mp4 > /dev/null 2>&1
done
swiftc -O -o dv_display_check "$D/dv_display_check.swift"
for c in A B C; do ./dv_display_check $c.mp4 $c.raw; done
./dv_display_check B.mp4 B2.raw  # repeat render: the noise floor
$PY "$D/dv_patch_check.py" score patches.json A.raw B.raw C.raw B2.raw > patch-check.json
head -20 patch-check.json
